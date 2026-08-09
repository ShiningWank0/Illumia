use std::{
    fs,
    io::Cursor,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use illumia_core::{
    assets::AssetService,
    db::{Database, Error, Result},
    stacks::StackService,
    thumbnails::generate_thumbnails,
    timeline::{Granularity, TimelineService},
    vault::{
        KdfParams, VaultHandle, change_password_with_kdf, export_assets, import_assets,
        import_stack, init_with_kdf, unlock, unlock_with_recovery,
    },
};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use tempfile::TempDir;

fn png(seed: u8) -> Vec<u8> {
    let mut pixels = RgbaImage::new(8, 6);
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        *pixel = Rgba([
            seed.wrapping_add(u8::try_from(index).unwrap_or_default()),
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

struct Fixture {
    _directory: TempDir,
    main: Database,
    vault: VaultHandle,
    recovery: String,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let main = Database::open(directory.path())?;
        let recovery = init_with_kdf(directory.path(), "correct horse", KdfParams::for_tests())?;
        let vault =
            VaultHandle::open(directory.path(), unlock(directory.path(), "correct horse")?)?;
        Ok(Self {
            _directory: directory,
            main,
            vault,
            recovery,
        })
    }

    fn root(&self) -> &Path {
        self.main.data_root()
    }
}

#[test]
fn password_and_recovery_unlock_have_correct_failure_semantics() -> Result<()> {
    let fixture = Fixture::new()?;
    assert!(unlock(fixture.root(), "correct horse").is_ok());
    assert!(matches!(
        unlock(fixture.root(), "wrong password"),
        Err(Error::VaultAuthenticationFailed)
    ));
    assert!(unlock_with_recovery(fixture.root(), &fixture.recovery).is_ok());
    assert!(matches!(
        unlock_with_recovery(fixture.root(), "AAAAAAAA"),
        Err(Error::InvalidRecoveryKey)
    ));
    Ok(())
}

#[test]
fn password_change_rewraps_mk_without_reencrypting_blob() -> Result<()> {
    let fixture = Fixture::new()?;
    let bytes = vec![42_u8; 1024 * 1024 + 17];
    let blob_id = fixture.vault.write_blob(&bytes)?;
    let encrypted_before = fs::read(fixture.root().join("vault").join("blobs").join(&blob_id))?;

    change_password_with_kdf(
        fixture.root(),
        "correct horse",
        "new password",
        KdfParams::for_tests(),
    )?;
    assert!(matches!(
        unlock(fixture.root(), "correct horse"),
        Err(Error::VaultAuthenticationFailed)
    ));
    let reopened = VaultHandle::open(fixture.root(), unlock(fixture.root(), "new password")?)?;
    assert_eq!(reopened.read_blob(&blob_id)?, bytes);
    let recovered = VaultHandle::open(
        fixture.root(),
        unlock_with_recovery(fixture.root(), &fixture.recovery)?,
    )?;
    assert_eq!(recovered.read_blob(&blob_id)?, bytes);
    assert_eq!(
        fs::read(fixture.root().join("vault").join("blobs").join(blob_id))?,
        encrypted_before
    );
    Ok(())
}

#[test]
fn blob_roundtrip_and_aead_tamper_and_aad_checks() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut bytes = vec![0_u8; 2 * 1024 * 1024 + 31];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from(index % 251).unwrap_or_default();
    }
    let first = fixture.vault.write_blob(&bytes)?;
    assert_eq!(fixture.vault.read_blob(&first)?, bytes);
    let chunks = fixture
        .vault
        .blob_reader(&first)?
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        [1024 * 1024, 1024 * 1024, 31]
    );
    assert_eq!(chunks.concat(), bytes);

    let path = fixture.root().join("vault").join("blobs").join(&first);
    let mut encrypted = fs::read(&path)?;
    let last = encrypted.len() - 1;
    encrypted[last] ^= 1;
    fs::write(&path, encrypted)?;
    assert!(matches!(
        fixture.vault.read_blob(&first),
        Err(Error::InvalidVaultBlob)
    ));

    let source = fixture.vault.write_blob(b"aad source")?;
    let destination = fixture.vault.write_blob(b"aad destination")?;
    let source_bytes = fs::read(fixture.root().join("vault").join("blobs").join(source))?;
    assert!(!contains_bytes(&source_bytes, b"aad source"));
    fs::write(
        fixture
            .root()
            .join("vault")
            .join("blobs")
            .join(&destination),
        source_bytes,
    )?;
    assert!(fixture.vault.read_blob(&destination).is_err());
    Ok(())
}

#[test]
fn revoking_one_handle_invalidates_all_key_clones_and_open_readers() -> Result<()> {
    let fixture = Fixture::new()?;
    let blob_id = fixture.vault.write_blob(&vec![7_u8; 2 * 1024 * 1024])?;
    let clone = fixture.vault.clone();
    let mut reader = clone.blob_reader(&blob_id)?;
    assert!(reader.next().transpose()?.is_some());

    fixture.vault.revoke();
    assert!(matches!(
        clone.ensure_active(),
        Err(Error::VaultAuthenticationFailed)
    ));
    assert!(matches!(
        reader.next(),
        Some(Err(Error::VaultAuthenticationFailed))
    ));
    assert!(matches!(
        clone.read_blob(&blob_id),
        Err(Error::VaultAuthenticationFailed)
    ));
    Ok(())
}

#[test]
fn vault_database_rejects_keyless_sqlite_access() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.vault.db.with_connection(|connection| {
        connection.execute(
            "INSERT INTO settings(key, value) VALUES ('cipher-test', 'present')",
            [],
        )?;
        Ok(())
    })?;
    fixture.vault.db.checkpoint_truncate()?;

    let connection = rusqlite::Connection::open(fixture.root().join("vault").join("vault.db"))?;
    assert!(
        connection
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_err()
    );
    Ok(())
}

#[test]
fn import_removes_every_plaintext_trace_and_core_services_use_vault_db() -> Result<()> {
    let fixture = Fixture::new()?;
    let bytes = png(11);
    let asset = AssetService::new(fixture.main.clone())
        .ingest(&bytes, "秘密作品.png", None)?
        .asset;
    generate_thumbnails(&fixture.main, &asset.id)?;
    fixture.main.with_connection(|connection| {
        connection.execute(
            "INSERT INTO clusters(id, name, created_by, created_at)
             VALUES ('cluster-secret', '秘密人物', 'user', '2026-01-01T00:00:00Z')",
            [],
        )?;
        connection.execute(
            "INSERT INTO faces(
               id, asset_id, kind, bbox, det_conf, model_version, cluster_id
             ) VALUES (
               'face-secret', ?1, 'face', '[0,0,1,1]', 1.0, 'test', 'cluster-secret'
             )",
            [&asset.id],
        )?;
        connection.execute(
            "INSERT INTO jobs(id, kind, payload, created_at)
             VALUES ('job-secret', 'thumbnail', json_object('asset_id', ?1),
                     '2026-01-01T00:00:00Z')",
            [&asset.id],
        )?;
        Ok(())
    })?;
    let original_path = fixture.root().join(&asset.library_path);
    let thumbnail_path = fixture
        .root()
        .join("thumbs")
        .join(format!("{}_t.webp", asset.id));
    let preview_path = fixture
        .root()
        .join("thumbs")
        .join(format!("{}_p.webp", asset.id));

    import_assets(
        &fixture.main,
        &fixture.vault,
        std::slice::from_ref(&asset.id),
    )?;

    assert!(!original_path.exists());
    assert!(!thumbnail_path.exists());
    assert!(!preview_path.exists());
    assert_plaintext_database_has_no_trace(&fixture.main, &asset.id)?;
    let vault_asset = AssetService::new(fixture.vault.db.clone())
        .get(&asset.id)?
        .expect("asset should be present in vault");
    assert_ne!(vault_asset.library_path, asset.library_path);
    assert_eq!(
        TimelineService::new(fixture.vault.db.clone()).buckets(Granularity::Day)?,
        vec![(asset.taken_at_local_date[..10].to_owned(), 1)]
    );
    assert_eq!(fixture.vault.read_blob(&vault_asset.library_path)?, bytes);
    assert_eq!(vault_blob_count(&fixture.vault.db, &asset.id)?, 3);
    Ok(())
}

#[test]
fn import_rejects_partial_duplicate_closure_before_deleting_source_files() -> Result<()> {
    let fixture = Fixture::new()?;
    let bytes = png(12);
    let service = AssetService::new(fixture.main.clone());
    let canonical = service.ingest(&bytes, "canonical.png", None)?.asset;
    let duplicate = service.ingest(&bytes, "duplicate.png", None)?.asset;
    let canonical_path = fixture.root().join(&canonical.library_path);
    let duplicate_path = fixture.root().join(&duplicate.library_path);

    assert!(matches!(
        import_assets(
            &fixture.main,
            &fixture.vault,
            std::slice::from_ref(&canonical.id)
        ),
        Err(Error::IncompleteVaultTransfer)
    ));
    assert!(canonical_path.is_file());
    assert!(duplicate_path.is_file());
    assert!(service.get(&canonical.id)?.is_some());
    assert_eq!(
        service
            .get(&duplicate.id)?
            .and_then(|asset| asset.duplicate_of),
        Some(canonical.id.clone())
    );
    assert!(
        AssetService::new(fixture.vault.db.clone())
            .get(&canonical.id)?
            .is_none()
    );

    import_assets(
        &fixture.main,
        &fixture.vault,
        &[canonical.id.clone(), duplicate.id.clone()],
    )?;
    assert!(service.get(&canonical.id)?.is_none());
    assert!(service.get(&duplicate.id)?.is_none());
    assert!(matches!(
        export_assets(
            &fixture.vault,
            &fixture.main,
            std::slice::from_ref(&canonical.id)
        ),
        Err(Error::IncompleteVaultTransfer)
    ));
    assert!(
        AssetService::new(fixture.vault.db.clone())
            .get(&canonical.id)?
            .is_some()
    );
    Ok(())
}

#[test]
fn import_reselects_shared_cluster_cover_without_plaintext_vault_face_id() -> Result<()> {
    let fixture = Fixture::new()?;
    let service = AssetService::new(fixture.main.clone());
    let moved = service.ingest(&png(13), "moved.png", None)?.asset;
    let remaining = service.ingest(&png(14), "remaining.png", None)?.asset;
    fixture.main.with_connection(|connection| {
        connection.execute(
            "INSERT INTO clusters(id, name, cover_face_id, created_by, created_at)
             VALUES ('shared', 'shared', 'moved-face', 'user', '2026-01-01T00:00:00Z')",
            [],
        )?;
        for (face_id, asset_id) in [
            ("moved-face", moved.id.as_str()),
            ("remaining-face", remaining.id.as_str()),
        ] {
            connection.execute(
                "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, model_version,
                                   cluster_id, state)
                 VALUES (?1, ?2, 'face', '[0,0,1,1]', 1.0, 'test', 'shared', 'confirmed')",
                rusqlite::params![face_id, asset_id],
            )?;
        }
        Ok(())
    })?;

    import_assets(
        &fixture.main,
        &fixture.vault,
        std::slice::from_ref(&moved.id),
    )?;

    let source_cover = fixture.main.with_connection(|connection| {
        connection
            .query_row(
                "SELECT cover_face_id FROM clusters WHERE id = 'shared'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(Into::into)
    })?;
    assert_eq!(source_cover.as_deref(), Some("remaining-face"));
    let destination_cover = fixture.vault.db.with_connection(|connection| {
        connection
            .query_row(
                "SELECT cover_face_id FROM clusters WHERE id = 'shared'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(Into::into)
    })?;
    assert_eq!(destination_cover.as_deref(), Some("moved-face"));
    Ok(())
}

#[test]
fn direct_vault_ingest_never_persists_plaintext_in_main_storage() -> Result<()> {
    let fixture = Fixture::new()?;
    let bytes = png(19);
    let original_name = "直接暗号化作品.png";

    let ingested = fixture.vault.ingest(&bytes, original_name, None)?;

    assert!(
        AssetService::new(fixture.main.clone())
            .get(&ingested.asset.id)?
            .is_none()
    );
    assert_eq!(
        fixture.vault.read_blob(&ingested.asset.library_path)?,
        bytes
    );
    assert!(
        !fixture.root().join("library").exists()
            || fs::read_dir(fixture.root().join("library"))?
                .next()
                .is_none()
    );
    assert!(
        !fixture.root().join("thumbs").exists()
            || fs::read_dir(fixture.root().join("thumbs"))?
                .next()
                .is_none()
    );
    fixture.main.checkpoint_truncate()?;
    let database_bytes = fs::read(fixture.root().join("illumia.db"))?;
    assert!(!contains_bytes(
        &database_bytes,
        ingested.asset.id.as_bytes()
    ));
    assert!(!contains_bytes(&database_bytes, original_name.as_bytes()));
    let staging = fixture.root().join("vault").join("transfer-staging");
    assert!(!staging.exists() || fs::read_dir(staging)?.next().is_none());
    Ok(())
}

#[test]
fn missing_thumbnails_are_generated_inside_vault() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = AssetService::new(fixture.main.clone())
        .ingest(&png(22), "thumbnail-source.png", None)?
        .asset;
    import_assets(
        &fixture.main,
        &fixture.vault,
        std::slice::from_ref(&asset.id),
    )?;
    assert_eq!(vault_blob_count(&fixture.vault.db, &asset.id)?, 1);

    fixture.vault.generate_thumbnails(&asset.id)?;
    assert_eq!(vault_blob_count(&fixture.vault.db, &asset.id)?, 3);
    assert!(
        AssetService::new(fixture.vault.db.clone())
            .get(&asset.id)?
            .expect("vault asset")
            .thumbhash
            .is_some()
    );
    Ok(())
}

#[test]
fn whole_stack_import_preserves_stack_and_pages() -> Result<()> {
    let fixture = Fixture::new()?;
    let service = AssetService::new(fixture.main.clone());
    let first = service.ingest(&png(31), "page-one.png", None)?.asset;
    let second = service.ingest(&png(32), "page-two.png", None)?.asset;
    let stack = StackService::new(fixture.main.clone())
        .create("秘密漫画", &[first.id.clone(), second.id.clone()])?;

    import_stack(&fixture.main, &fixture.vault, &stack.id)?;
    assert!(
        StackService::new(fixture.main.clone())
            .get(&stack.id)?
            .is_none()
    );
    let imported = StackService::new(fixture.vault.db.clone())
        .get(&stack.id)?
        .expect("stack should be present in vault");
    assert_eq!(imported.chapters[0].pages.len(), 2);
    assert_eq!(
        StackService::new(fixture.vault.db.clone())
            .search("秘密漫画")?
            .first()
            .map(|item| item.id.as_str()),
        Some(stack.id.as_str())
    );
    Ok(())
}

#[test]
fn export_removes_every_vault_trace_and_restores_plaintext_asset() -> Result<()> {
    let fixture = Fixture::new()?;
    let bytes = png(41);
    let asset = AssetService::new(fixture.main.clone())
        .ingest(&bytes, "export-target.png", None)?
        .asset;
    generate_thumbnails(&fixture.main, &asset.id)?;
    import_assets(
        &fixture.main,
        &fixture.vault,
        std::slice::from_ref(&asset.id),
    )?;

    export_assets(
        &fixture.vault,
        &fixture.main,
        std::slice::from_ref(&asset.id),
    )?;

    let exported = AssetService::new(fixture.main.clone())
        .get(&asset.id)?
        .expect("asset should be restored");
    assert_eq!(fs::read(fixture.root().join(exported.library_path))?, bytes);
    assert!(
        AssetService::new(fixture.vault.db.clone())
            .get(&asset.id)?
            .is_none()
    );
    assert_eq!(vault_blob_count(&fixture.vault.db, &asset.id)?, 0);
    assert!(
        fs::read_dir(fixture.root().join("vault").join("blobs"))?
            .next()
            .is_none()
    );
    assert_eq!(fts_count(&fixture.vault.db, &asset.id)?, 0);
    Ok(())
}

#[test]
fn export_writes_plaintext_before_waiting_for_the_main_database_mutex() -> Result<()> {
    let fixture = Fixture::new()?;
    let asset = AssetService::new(fixture.main.clone())
        .ingest(&png(151), "export-lock-order.png", None)?
        .asset;
    import_assets(
        &fixture.main,
        &fixture.vault,
        std::slice::from_ref(&asset.id),
    )?;

    let locked_database = fixture.main.clone();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let locker = thread::spawn(move || {
        locked_database.with_connection(|_| {
            locked_tx.send(()).expect("signal held main database mutex");
            release_rx.recv().expect("release main database mutex");
            Ok(())
        })
    });
    locked_rx
        .recv()
        .expect("main database mutex should be held");

    let vault = fixture.vault.clone();
    let main = fixture.main.clone();
    let id = asset.id.clone();
    let export = thread::spawn(move || export_assets(&vault, &main, &[id]));
    let library = fixture.root().join("library");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !contains_regular_file(&library)? {
        assert!(
            Instant::now() < deadline,
            "decrypt/write must finish before export waits for the main SQLite mutex"
        );
        thread::sleep(Duration::from_millis(10));
    }

    release_tx.send(()).expect("release main database mutex");
    locker.join().expect("locker thread")?;
    export.join().expect("export thread")?;
    Ok(())
}

fn contains_regular_file(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            || (entry.file_type()?.is_dir() && contains_regular_file(&entry.path())?)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn assert_plaintext_database_has_no_trace(database: &Database, asset_id: &str) -> Result<()> {
    database.with_connection(|connection| {
        for (table, column) in [
            ("assets", "id"),
            ("faces", "asset_id"),
            ("stack_pages", "asset_id"),
            ("search_fts", "entity_id"),
        ] {
            let sql = format!("SELECT count(*) FROM {table} WHERE {column} = ?1");
            assert_eq!(
                connection.query_row(&sql, [asset_id], |row| row.get::<_, i64>(0))?,
                0
            );
        }
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM clusters WHERE id = 'cluster-secret'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM jobs WHERE id = 'job-secret'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM search_fts WHERE search_fts MATCH '秘密作品'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })?;
    let wal = database.data_root().join("illumia.db-wal");
    assert!(!wal.exists() || fs::metadata(wal)?.len() == 0);
    let database_bytes = fs::read(database.data_root().join("illumia.db"))?;
    assert!(!contains_bytes(&database_bytes, asset_id.as_bytes()));
    assert!(!contains_bytes(&database_bytes, "秘密作品".as_bytes()));
    assert!(!contains_bytes(&database_bytes, "秘密人物".as_bytes()));
    Ok(())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn vault_blob_count(database: &Database, asset_id: &str) -> Result<i64> {
    database.with_connection(|connection| {
        connection
            .query_row(
                "SELECT count(*) FROM vault_blobs WHERE asset_id = ?1",
                [asset_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    })
}

fn fts_count(database: &Database, asset_id: &str) -> Result<i64> {
    database.with_connection(|connection| {
        connection
            .query_row(
                "SELECT count(*) FROM search_fts WHERE entity_id = ?1",
                [asset_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    })
}
