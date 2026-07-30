use std::io::Cursor;

use chrono::{Duration, TimeZone, Utc};
use illumia_core::{
    PurgeService,
    assets::{Asset, AssetService, Lifecycle},
    db::{Database, Error, Result},
    stacks::{ChapterInput, StackService},
};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    database: Database,
    assets: AssetService,
    stacks: StackService,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let database = Database::open(directory.path())?;
        Ok(Self {
            assets: AssetService::new(database.clone()),
            stacks: StackService::new(database.clone()),
            database,
            _directory: directory,
        })
    }

    fn ingest(&self, seed: u8, name: &str) -> Asset {
        let uploaded = Utc
            .with_ymd_and_hms(2026, 7, 30, 12, 0, u32::from(seed))
            .single()
            .expect("valid timestamp");
        self.assets
            .ingest_at(&png(seed), name, Some(uploaded), uploaded)
            .expect("asset should be ingested")
            .asset
    }
}

#[test]
fn full_structure_edit_flow_preserves_flags_and_recomputes_visibility() -> Result<()> {
    let fixture = Fixture::new()?;
    let first = fixture.ingest(1, "first.png");
    let second = fixture.ingest(2, "second.png");
    let third = fixture.ingest(3, "third.png");

    let created = fixture.stacks.create(
        "連載",
        &[first.id.clone(), second.id.clone(), third.id.clone()],
    )?;
    assert_eq!(created.chapters.len(), 1);
    assert_eq!(
        visible(&fixture.database, &[&first.id, &second.id, &third.id])?,
        vec![false, false, false]
    );

    fixture
        .stacks
        .set_page_flag(&created.id, &second.id, true)?;
    assert_eq!(
        visible(&fixture.database, &[&first.id, &second.id, &third.id])?,
        vec![false, true, false]
    );

    let replaced = fixture.stacks.replace_structure(
        &created.id,
        &[
            ChapterInput {
                title: Some("前編".to_owned()),
                pages: vec![third.id.clone(), second.id.clone()],
            },
            ChapterInput {
                title: None,
                pages: vec![first.id.clone()],
            },
        ],
    )?;
    assert_eq!(
        replaced
            .chapters
            .iter()
            .map(|chapter| chapter.chapter_no)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        replaced.chapters[0]
            .pages
            .iter()
            .map(|page| (page.asset.id.as_str(), page.page_no))
            .collect::<Vec<_>>(),
        vec![(third.id.as_str(), 1), (second.id.as_str(), 2)]
    );
    assert_eq!(replaced.chapters[1].pages[0].page_no, 1);
    assert!(replaced.chapters[0].pages[1].show_in_timeline);
    assert_eq!(
        visible(&fixture.database, &[&first.id, &second.id, &third.id])?,
        vec![false, true, false]
    );

    let before_order = page_order(&fixture.stacks, &created.id)?;
    fixture.stacks.set_page_flag(&created.id, &third.id, true)?;
    assert_eq!(page_order(&fixture.stacks, &created.id)?, before_order);
    assert_eq!(
        visible(&fixture.database, &[&first.id, &second.id, &third.id])?,
        vec![false, true, true]
    );

    fixture.stacks.remove_page(&created.id, &first.id)?;
    assert!(visible(&fixture.database, &[&first.id])?[0]);
    fixture.stacks.delete_stack(&created.id)?;
    assert_eq!(
        visible(&fixture.database, &[&first.id, &second.id, &third.id])?,
        vec![true, true, true]
    );
    assert!(fixture.assets.get(&first.id)?.is_some());
    assert!(fixture.assets.get(&second.id)?.is_some());
    assert!(fixture.assets.get(&third.id)?.is_some());
    Ok(())
}

#[test]
fn duplicate_page_is_promoted_and_stack_reference_still_enforces_i3() -> Result<()> {
    let fixture = Fixture::new()?;
    let uploaded = Utc
        .with_ymd_and_hms(2026, 7, 30, 12, 0, 0)
        .single()
        .expect("valid timestamp");
    let bytes = png(20);
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
    assert_eq!(duplicate.lifecycle, Lifecycle::Duplicate);

    let stack = fixture
        .stacks
        .create("重複ページ", std::slice::from_ref(&duplicate.id))?;
    let promoted = fixture
        .assets
        .get(&duplicate.id)?
        .expect("promoted duplicate should remain");
    assert_eq!(promoted.lifecycle, Lifecycle::Active);
    assert_eq!(promoted.duplicate_of.as_deref(), Some(original.id.as_str()));
    assert!(promoted.purge_after.is_none());
    assert!(!promoted.visible_in_timeline);

    fixture.database.with_connection(|connection| {
        connection.execute(
            "UPDATE assets
             SET lifecycle = 'duplicate', purge_after = ?2
             WHERE id = ?1",
            rusqlite::params![duplicate.id, (uploaded - Duration::seconds(1)).to_rfc3339()],
        )?;
        Ok(())
    })?;
    assert_eq!(
        PurgeService::new(fixture.database.clone()).run_due_at(uploaded)?,
        0
    );
    assert!(fixture.assets.get(&duplicate.id)?.is_some());
    assert!(fixture.stacks.get(&stack.id)?.is_some());
    Ok(())
}

#[test]
fn structure_replacement_rejects_duplicate_and_missing_assets() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = fixture.ingest(30, "page.png");
    let stack = fixture
        .stacks
        .create("検証", std::slice::from_ref(&asset.id))?;

    let duplicate = fixture.stacks.replace_structure(
        &stack.id,
        &[ChapterInput {
            title: None,
            pages: vec![asset.id.clone(), asset.id.clone()],
        }],
    );
    assert!(matches!(duplicate, Err(Error::InvalidStack(_))));

    let missing = fixture.stacks.replace_structure(
        &stack.id,
        &[ChapterInput {
            title: None,
            pages: vec!["missing-asset".to_owned()],
        }],
    );
    assert!(matches!(missing, Err(Error::InvalidStack(_))));
    assert_eq!(page_order(&fixture.stacks, &stack.id)?, vec![asset.id]);
    Ok(())
}

#[test]
fn asset_is_visible_only_when_every_stack_membership_is_enabled() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = fixture.ingest(40, "shared.png");
    let first = fixture
        .stacks
        .create("第一作品", std::slice::from_ref(&asset.id))?;
    let second = fixture
        .stacks
        .create("第二作品", std::slice::from_ref(&asset.id))?;

    fixture.stacks.set_page_flag(&first.id, &asset.id, true)?;
    assert!(!visible(&fixture.database, &[&asset.id])?[0]);
    fixture.stacks.set_page_flag(&second.id, &asset.id, true)?;
    assert!(visible(&fixture.database, &[&asset.id])?[0]);
    fixture.stacks.set_page_flag(&first.id, &asset.id, false)?;
    assert!(!visible(&fixture.database, &[&asset.id])?[0]);
    fixture.stacks.delete_stack(&first.id)?;
    assert!(visible(&fixture.database, &[&asset.id])?[0]);
    Ok(())
}

fn png(seed: u8) -> Vec<u8> {
    let mut pixels = RgbaImage::new(2, 2);
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

fn visible(database: &Database, ids: &[&str]) -> Result<Vec<bool>> {
    database.with_connection(|connection| {
        ids.iter()
            .map(|id| {
                connection
                    .query_row(
                        "SELECT visible_in_timeline FROM assets WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .collect()
    })
}

fn page_order(stacks: &StackService, id: &str) -> Result<Vec<String>> {
    Ok(stacks
        .get(id)?
        .ok_or(Error::StackNotFound)?
        .chapters
        .into_iter()
        .flat_map(|chapter| chapter.pages)
        .map(|page| page.asset.id)
        .collect())
}
