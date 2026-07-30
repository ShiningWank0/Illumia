use std::{fs, io::Cursor, path::PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use illumia_core::{
    PurgeService,
    assets::{Asset, AssetService, Lifecycle},
    db::{Database, Error, Result},
    settings::Settings,
    stacks::StackService,
    timeline::{Granularity, TimelineService},
};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    database: Database,
    assets: AssetService,
    purge: PurgeService,
    stacks: StackService,
    timeline: TimelineService,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let database = Database::open(directory.path())?;
        Ok(Self {
            assets: AssetService::new(database.clone()),
            purge: PurgeService::new(database.clone()),
            stacks: StackService::new(database.clone()),
            timeline: TimelineService::new(database.clone()),
            database,
            _directory: directory,
        })
    }

    fn ingest(&self, seed: u8, name: &str, taken_at: DateTime<Utc>) -> Asset {
        self.assets
            .ingest_at(&png(seed), name, Some(taken_at), taken_at)
            .expect("asset should be ingested")
            .asset
    }

    fn absolute_path(&self, asset: &Asset) -> PathBuf {
        self.database.data_root().join(&asset.library_path)
    }

    fn force_expired(&self, id: &str, lifecycle: &str, when: DateTime<Utc>) {
        self.database
            .with_connection(|connection| {
                connection.execute(
                    "UPDATE assets
                     SET lifecycle = ?2, purge_after = ?3, visible_in_timeline = 0
                     WHERE id = ?1",
                    rusqlite::params![id, lifecycle, rfc3339(when)],
                )?;
                Ok(())
            })
            .expect("expiry should be forced");
    }
}

fn png(seed: u8) -> Vec<u8> {
    let mut pixels = RgbaImage::new(3, 2);
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        *pixel = Rgba([
            seed.wrapping_add(u8::try_from(index).expect("small image")),
            seed.wrapping_mul(3),
            255_u8.wrapping_sub(seed),
            255,
        ]);
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(pixels)
        .write_to(&mut output, ImageFormat::Png)
        .expect("PNG should encode");
    output.into_inner()
}

fn instant(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .expect("valid timestamp")
}

fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[test]
fn i1_active_assets_are_never_purged() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 1, 1, 0);
    let asset = fixture.ingest(1, "active.png", uploaded);
    let path = fixture.absolute_path(&asset);

    assert_eq!(fixture.purge.run_due_at(uploaded + Duration::days(365))?, 0);
    assert!(path.is_file());
    assert_eq!(
        fixture.assets.get(&asset.id)?.map(|item| item.lifecycle),
        Some(Lifecycle::Active)
    );
    Ok(())
}

#[test]
fn i2_only_later_duplicate_is_purged_property_cases() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 2, 1, 0);
    let mut originals = Vec::new();

    // 依存追加なしで、複数の hash / upload 順に対する property-style 検証を行う。
    for seed in 10..18 {
        let bytes = png(seed);
        let original = fixture
            .assets
            .ingest_at(&bytes, "original.png", Some(uploaded), uploaded)?
            .asset;
        let duplicate = fixture
            .assets
            .ingest_at(
                &bytes,
                "duplicate.png",
                Some(uploaded),
                uploaded + Duration::seconds(1),
            )?
            .asset;
        assert_eq!(
            duplicate.duplicate_of.as_deref(),
            Some(original.id.as_str())
        );
        fixture.force_expired(&duplicate.id, "duplicate", uploaded - Duration::seconds(1));
        originals.push((original, duplicate));
    }

    assert_eq!(fixture.purge.run_due_at(uploaded)?, originals.len());
    for (original, duplicate) in originals {
        assert!(fixture.assets.get(&original.id)?.is_some());
        assert!(fixture.absolute_path(&original).is_file());
        assert!(fixture.assets.get(&duplicate.id)?.is_none());
        assert!(!fixture.absolute_path(&duplicate).exists());
    }
    Ok(())
}

#[test]
fn i3_stack_references_block_duplicate_and_trashed_purge() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 3, 1, 0);
    let duplicate_bytes = png(30);
    let _original =
        fixture
            .assets
            .ingest_at(&duplicate_bytes, "original.png", Some(uploaded), uploaded)?;
    let duplicate = fixture
        .assets
        .ingest_at(&duplicate_bytes, "duplicate.png", Some(uploaded), uploaded)?
        .asset;
    let trashed = fixture.ingest(31, "trashed.png", uploaded);
    fixture
        .stacks
        .create("保護スタック", &[duplicate.id.clone(), trashed.id.clone()])?;

    // duplicate の昇格漏れを意図的に再現しても SQL の NOT EXISTS が防ぐ。
    fixture.force_expired(&duplicate.id, "duplicate", uploaded - Duration::seconds(1));
    fixture.assets.trash_at(&trashed.id, uploaded)?;
    fixture.force_expired(&trashed.id, "trashed", uploaded - Duration::seconds(1));

    assert_eq!(fixture.purge.run_due_at(uploaded)?, 0);
    assert!(fixture.assets.get(&duplicate.id)?.is_some());
    assert!(fixture.assets.get(&trashed.id)?.is_some());
    assert!(fixture.absolute_path(&duplicate).is_file());
    assert!(fixture.absolute_path(&trashed).is_file());
    Ok(())
}

#[test]
fn i4_retrash_resets_timer_from_current_operation() -> Result<()> {
    let fixture = Fixture::new()?;
    Settings::new(fixture.database.clone()).set_trash_retention_days(12)?;
    let first_delete = instant(2025, 4, 1, 0);
    let restore = first_delete + Duration::days(3);
    let second_delete = first_delete + Duration::days(8);
    let asset = fixture.ingest(40, "timer.png", first_delete);

    let first = fixture.assets.trash_at(&asset.id, first_delete)?;
    fixture.assets.restore_at(&asset.id, restore)?;
    let second = fixture.assets.trash_at(&asset.id, second_delete)?;

    assert_eq!(
        first.purge_after.as_deref(),
        Some(rfc3339(first_delete + Duration::days(12)).as_str())
    );
    assert_eq!(
        second.purge_after.as_deref(),
        Some(rfc3339(second_delete + Duration::days(12)).as_str())
    );
    assert_ne!(first.purge_after, second.purge_after);
    assert_eq!(
        second.trashed_at.as_deref(),
        Some(rfc3339(second_delete).as_str())
    );
    Ok(())
}

#[test]
fn i5_purge_deletes_only_the_rows_owned_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 5, 1, 0);
    let bytes = png(50);
    let original = fixture
        .assets
        .ingest_at(&bytes, "same.png", Some(uploaded), uploaded)?
        .asset;
    let duplicate = fixture
        .assets
        .ingest_at(&bytes, "same-copy.png", Some(uploaded), uploaded)?
        .asset;
    let original_path = fixture.absolute_path(&original);
    let duplicate_path = fixture.absolute_path(&duplicate);
    assert_ne!(original_path, duplicate_path);
    assert_eq!(fs::read(&original_path)?, fs::read(&duplicate_path)?);

    fixture.force_expired(&duplicate.id, "duplicate", uploaded - Duration::seconds(1));
    fixture.purge.run_due_at(uploaded)?;

    assert_eq!(fs::read(&original_path)?, bytes);
    assert!(!duplicate_path.exists());
    Ok(())
}

#[test]
fn purge_rejects_non_uuid_vault_blob_ids_before_deleting_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 5, 2, 0);
    let asset = fixture.ingest(51, "invalid-blob.png", uploaded);
    let asset_path = fixture.absolute_path(&asset);
    let vault_dir = fixture.database.data_root().join("vault");
    fs::create_dir_all(vault_dir.join("blobs"))?;
    let traversal_target = vault_dir.join("sentinel");
    fs::write(&traversal_target, b"must remain")?;
    fixture.database.with_connection(|connection| {
        connection.execute(
            "INSERT INTO vault_blobs(blob_id, wrapped_key, kind, asset_id)
             VALUES ('../sentinel', X'00', 'original', ?1)",
            [&asset.id],
        )?;
        Ok(())
    })?;
    fixture.assets.trash_at(&asset.id, uploaded)?;

    assert!(matches!(
        fixture.purge.purge_now(&asset.id),
        Err(Error::InvalidVaultBlob)
    ));
    assert!(asset_path.is_file());
    assert_eq!(fs::read(traversal_target)?, b"must remain");
    Ok(())
}

#[test]
fn i6_restore_recovers_timeline_search_and_stack_visibility() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 6, 15, 8);
    let asset = fixture.ingest(60, "星空作品.png", uploaded);
    let stack = fixture
        .stacks
        .create("作品スタック", std::slice::from_ref(&asset.id))?;
    fixture.stacks.set_page_flag(&stack.id, &asset.id, true)?;

    let before_buckets = fixture.timeline.buckets(Granularity::Day)?;
    let before_search = searchable_asset_count(&fixture.database, &asset.id)?;
    let before_stack = stack_page_count(&fixture.database, &asset.id)?;
    assert!(asset_is_visible(&fixture.database, &asset.id)?);

    fixture.assets.trash_at(&asset.id, uploaded)?;
    assert!(!asset_is_visible(&fixture.database, &asset.id)?);
    assert!(fixture.timeline.bucket_items("2025-06-15")?.is_empty());
    assert_eq!(searchable_asset_count(&fixture.database, &asset.id)?, 0);
    assert_eq!(
        stack_page_count(&fixture.database, &asset.id)?,
        before_stack
    );

    let restored = fixture
        .assets
        .restore_at(&asset.id, uploaded + Duration::hours(1))?;
    assert_eq!(restored.lifecycle, Lifecycle::Active);
    assert!(restored.visible_in_timeline);
    assert_eq!(fixture.timeline.buckets(Granularity::Day)?, before_buckets);
    assert_eq!(
        searchable_asset_count(&fixture.database, &asset.id)?,
        before_search
    );
    assert_eq!(
        stack_page_count(&fixture.database, &asset.id)?,
        before_stack
    );
    Ok(())
}

#[test]
fn ingest_detects_duplicates_and_keeps_separate_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 7, 1, 0);
    let bytes = png(70);
    let original = fixture
        .assets
        .ingest_at(&bytes, "first.PNG", Some(uploaded), uploaded)?;
    let duplicate = fixture.assets.ingest_at(
        &bytes,
        "second.png",
        Some(uploaded),
        uploaded + Duration::seconds(1),
    )?;

    assert_eq!(original.asset.lifecycle, Lifecycle::Active);
    assert_eq!(original.asset.width, 3);
    assert_eq!(original.asset.height, 2);
    assert_eq!(duplicate.asset.lifecycle, Lifecycle::Duplicate);
    assert_eq!(
        duplicate.duplicate_of.as_deref(),
        Some(original.asset.id.as_str())
    );
    assert!(!duplicate.asset.visible_in_timeline);
    assert_ne!(original.asset.library_path, duplicate.asset.library_path);
    assert!(fixture.absolute_path(&original.asset).is_file());
    assert!(fixture.absolute_path(&duplicate.asset).is_file());
    assert_eq!(fixture.assets.list_duplicates()?.len(), 1);
    Ok(())
}

#[test]
fn promoted_duplicate_does_not_violate_primary_hash_unique_index() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 8, 1, 0);
    let bytes = png(80);
    let original = fixture
        .assets
        .ingest_at(&bytes, "primary.png", Some(uploaded), uploaded)?
        .asset;
    let duplicate = fixture
        .assets
        .ingest_at(&bytes, "copy.png", Some(uploaded), uploaded)?
        .asset;
    fixture
        .stacks
        .create("重複昇格", std::slice::from_ref(&duplicate.id))?;
    let promoted = fixture
        .assets
        .get(&duplicate.id)?
        .expect("promoted asset should exist");
    assert_eq!(promoted.lifecycle, Lifecycle::Active);
    assert_eq!(promoted.duplicate_of.as_deref(), Some(original.id.as_str()));
    assert!(promoted.purge_after.is_none());

    let third = fixture
        .assets
        .ingest_at(&bytes, "third.png", Some(uploaded), uploaded)?
        .asset;
    assert_eq!(third.lifecycle, Lifecycle::Duplicate);
    assert_eq!(third.duplicate_of.as_deref(), Some(original.id.as_str()));
    let primary_count = fixture.database.with_connection(|connection| {
        connection
            .query_row(
                "SELECT count(*) FROM assets
                 WHERE hash = ?1 AND lifecycle = 'active' AND duplicate_of IS NULL",
                [&original.hash],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
    })?;
    assert_eq!(primary_count, 1);
    Ok(())
}

#[test]
fn restoring_old_primary_reparents_a_newer_same_hash_primary() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = instant(2025, 8, 2, 0);
    let bytes = png(81);
    let old_primary = fixture
        .assets
        .ingest_at(&bytes, "old.png", Some(uploaded), uploaded)?
        .asset;
    fixture.assets.trash_at(&old_primary.id, uploaded)?;

    let new_primary = fixture
        .assets
        .ingest_at(
            &bytes,
            "new.png",
            Some(uploaded),
            uploaded + Duration::hours(1),
        )?
        .asset;
    let newer_duplicate = fixture
        .assets
        .ingest_at(
            &bytes,
            "new-copy.png",
            Some(uploaded),
            uploaded + Duration::hours(2),
        )?
        .asset;
    assert_eq!(new_primary.lifecycle, Lifecycle::Active);
    assert!(new_primary.duplicate_of.is_none());
    assert_eq!(
        newer_duplicate.duplicate_of.as_deref(),
        Some(new_primary.id.as_str())
    );

    let restored = fixture
        .assets
        .restore_at(&old_primary.id, uploaded + Duration::hours(3))?;
    let reparented_primary = fixture
        .assets
        .get(&new_primary.id)?
        .expect("newer primary should remain");
    let reparented_duplicate = fixture
        .assets
        .get(&newer_duplicate.id)?
        .expect("newer duplicate should remain");

    assert_eq!(restored.lifecycle, Lifecycle::Active);
    assert!(restored.duplicate_of.is_none());
    assert_eq!(
        reparented_primary.duplicate_of.as_deref(),
        Some(old_primary.id.as_str())
    );
    assert_eq!(
        reparented_duplicate.duplicate_of.as_deref(),
        Some(old_primary.id.as_str())
    );
    Ok(())
}

#[test]
fn timeline_aggregates_day_month_and_year_buckets() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.ingest(90, "one.png", instant(2024, 12, 31, 23));
    let first = fixture.ingest(91, "two.png", instant(2025, 1, 2, 9));
    let second = fixture.ingest(92, "three.png", instant(2025, 1, 2, 11));
    fixture.ingest(93, "four.png", instant(2025, 2, 3, 10));

    assert_eq!(
        fixture.timeline.buckets(Granularity::Day)?,
        vec![
            ("2025-02-03".to_owned(), 1),
            ("2025-01-02".to_owned(), 2),
            ("2024-12-31".to_owned(), 1),
        ]
    );
    assert_eq!(
        fixture.timeline.buckets(Granularity::Month)?,
        vec![
            ("2025-02".to_owned(), 1),
            ("2025-01".to_owned(), 2),
            ("2024-12".to_owned(), 1),
        ]
    );
    assert_eq!(
        fixture.timeline.buckets(Granularity::Year)?,
        vec![("2025".to_owned(), 3), ("2024".to_owned(), 1)]
    );
    assert_eq!(
        fixture
            .timeline
            .bucket_items("2025-01-02")?
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![second.id, first.id]
    );
    Ok(())
}

#[test]
fn migration_and_typed_settings_are_configured() -> Result<()> {
    let fixture = Fixture::new()?;
    let settings = Settings::new(fixture.database.clone());
    assert_eq!(settings.trash_retention_days()?, 30);
    assert_eq!(settings.dedup_retention_days()?, 30);
    settings.set_dedup_retention_days(45)?;
    assert_eq!(settings.dedup_retention_days()?, 45);

    fixture.database.with_connection(|connection| {
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
            2
        );
        assert_eq!(
            connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
            1
        );
        let mode =
            connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        Ok(())
    })
}

fn asset_is_visible(database: &Database, id: &str) -> Result<bool> {
    database.with_connection(|connection| {
        connection
            .query_row(
                "SELECT visible_in_timeline FROM assets WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    })
}

fn stack_page_count(database: &Database, id: &str) -> Result<i64> {
    database.with_connection(|connection| {
        connection
            .query_row(
                "SELECT count(*) FROM stack_pages WHERE asset_id = ?1",
                [id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    })
}

fn searchable_asset_count(database: &Database, id: &str) -> Result<i64> {
    database.with_connection(|connection| {
        connection
            .query_row(
                "SELECT count(*)
                 FROM search_fts f
                 JOIN assets a ON a.id = f.entity_id
                 WHERE f.entity_type = 'asset'
                   AND f.search_fts MATCH '空作品'
                   AND a.id = ?1
                   AND a.lifecycle = 'active'",
                [id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    })
}
