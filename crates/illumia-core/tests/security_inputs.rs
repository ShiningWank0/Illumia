use std::{
    fs,
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use illumia_core::{
    assets::AssetService,
    db::{Database, Error, Result},
    search::SearchService,
    settings::{
        MAX_CLUSTER_SIZE_VALUE, MAX_JOB_CONCURRENCY, MAX_ML_CONCURRENCY, MAX_RETENTION_DAYS,
        Settings,
    },
    stacks::StackService,
    uuid::Uuid,
    vault::{KdfParams, VaultHandle, import_assets, init_with_kdf, unlock},
};
use tempfile::TempDir;

fn png(seed: u8) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([seed, 1, 2, 255]));
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .expect("test PNG should encode");
    output.into_inner()
}

#[test]
fn search_treats_sql_and_like_metacharacters_as_data() -> Result<()> {
    let directory = TempDir::new()?;
    let database = Database::open(directory.path())?;
    let assets = AssetService::new(database.clone());
    let marked = assets.ingest(&png(1), "100%_marker.png", None)?.asset;
    let ordinary = assets.ingest(&png(2), "ordinary.png", None)?.asset;
    let search = SearchService::new(database.clone());

    let percent = search.search("%")?;
    assert_eq!(
        percent
            .assets
            .iter()
            .map(|asset| asset.id.as_str())
            .collect::<Vec<_>>(),
        vec![marked.id.as_str()]
    );
    let underscore = search.search("_")?;
    assert_eq!(underscore.assets.len(), 1);
    assert_eq!(underscore.assets[0].id, marked.id);
    assert!(
        search
            .search("' OR 1=1 --")?
            .assets
            .iter()
            .all(|asset| asset.id != ordinary.id)
    );
    assert_eq!(search.search("ordinary")?.assets[0].id, ordinary.id);

    let stacks = StackService::new(database.clone());
    stacks.create("100%_stack", std::slice::from_ref(&marked.id))?;
    stacks.create("plain stack", std::slice::from_ref(&ordinary.id))?;
    let stack_result = search.search("%_")?;
    assert_eq!(stack_result.stacks.len(), 1);
    assert_eq!(stack_result.stacks[0].title, "100%_stack");
    assert!(search.search("' OR 1=1 --")?.stacks.is_empty());
    assert!(matches!(
        search.search(&"あ".repeat(257)),
        Err(Error::InvalidSearch)
    ));
    Ok(())
}

#[test]
fn settings_reject_resource_exhaustion_values_at_the_core_boundary() -> Result<()> {
    let directory = TempDir::new()?;
    let database = Database::open(directory.path())?;
    let settings = Settings::new(database.clone());

    assert!(settings.set_thumbnail_concurrency(0).is_err());
    assert!(
        settings
            .set_thumbnail_concurrency(MAX_JOB_CONCURRENCY + 1)
            .is_err()
    );
    assert!(settings.set_ml_concurrency(MAX_ML_CONCURRENCY + 1).is_err());
    assert!(
        settings
            .set_trash_retention_days(MAX_RETENTION_DAYS + 1)
            .is_err()
    );
    assert!(settings.set_tau_high_override(f64::NAN).is_err());
    assert!(settings.set_tau_low_override(-0.01).is_err());
    assert!(
        settings
            .set_min_cluster_size(MAX_CLUSTER_SIZE_VALUE + 1)
            .is_err()
    );

    database.with_connection(|connection| {
        connection.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)",
            ["jobs.thumbnail_concurrency", "4294967295"],
        )?;
        Ok(())
    })?;
    assert!(matches!(
        settings.thumbnail_concurrency(),
        Err(Error::InvalidSetting("jobs.thumbnail_concurrency"))
    ));
    Ok(())
}

#[test]
fn tampered_vault_kdf_and_oversized_transfers_fail_before_expensive_work() -> Result<()> {
    let directory = TempDir::new()?;
    init_with_kdf(
        directory.path(),
        "security password",
        KdfParams::for_tests(),
    )?;
    let keyfile_path = directory.path().join("vault").join("vault.keyfile");
    let mut keyfile: serde_json::Value = serde_json::from_slice(&fs::read(&keyfile_path)?)?;
    keyfile["kdf"]["memory_kib"] = serde_json::json!(4_000_000_u32);
    fs::write(&keyfile_path, serde_json::to_vec(&keyfile)?)?;
    assert!(matches!(
        unlock(directory.path(), "security password"),
        Err(Error::InvalidVaultKeyFile)
    ));

    let main_directory = TempDir::new()?;
    let vault_directory = TempDir::new()?;
    init_with_kdf(
        vault_directory.path(),
        "another security password",
        KdfParams::for_tests(),
    )?;
    let vault = VaultHandle::open(
        vault_directory.path(),
        unlock(vault_directory.path(), "another security password")?,
    )?;
    let main = Database::open(main_directory.path())?;
    let ids = (0..=illumia_core::vault::MAX_VAULT_TRANSFER_ASSETS)
        .map(|_| Uuid::now_v7().to_string())
        .collect::<Vec<_>>();
    assert!(matches!(
        import_assets(&main, &vault, &ids),
        Err(Error::EmptyVaultTransfer)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn data_root_databases_and_keyfile_are_owner_only() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new()?;
    let data_root = directory.path().join("data");
    let database = Database::open(&data_root)?;
    assert_eq!(
        fs::metadata(&data_root)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(data_root.join("illumia.db"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    init_with_kdf(
        &data_root,
        "filesystem security password",
        KdfParams::for_tests(),
    )?;
    let vault = VaultHandle::open(
        &data_root,
        unlock(&data_root, "filesystem security password")?,
    )?;
    drop(vault);
    assert_eq!(
        fs::metadata(data_root.join("vault"))?.permissions().mode() & 0o777,
        0o700
    );
    for path in [
        data_root.join("vault").join("vault.db"),
        data_root.join("vault").join("vault.keyfile"),
    ] {
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    }
    drop(database);
    Ok(())
}

#[test]
fn guarded_database_rechecks_revocation_after_waiting_for_the_mutex() -> Result<()> {
    let directory = TempDir::new()?;
    let database = Database::open(directory.path())?;
    let locked_database = database.clone();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let locker = thread::spawn(move || {
        locked_database.with_connection(|_| {
            locked_tx.send(()).expect("signal database mutex");
            release_rx.recv().expect("release database mutex");
            Ok(())
        })
    });
    locked_rx.recv().expect("database mutex should be held");

    let active = Arc::new(AtomicBool::new(true));
    let checks = Arc::new(AtomicUsize::new(0));
    let (first_check_tx, first_check_rx) = mpsc::channel();
    let guard_active = Arc::clone(&active);
    let guard_checks = Arc::clone(&checks);
    let guarded = database.with_access_guard(move || {
        if guard_checks.fetch_add(1, Ordering::AcqRel) == 0 {
            first_check_tx.send(()).expect("signal first guard check");
        }
        if guard_active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(Error::VaultAuthenticationFailed)
        }
    });
    let mutation = thread::spawn(move || {
        guarded.with_connection(|connection| {
            connection.execute(
                "UPDATE settings SET value = '999' WHERE key = 'trash.retention_days'",
                [],
            )?;
            Ok(())
        })
    });
    first_check_rx
        .recv()
        .expect("operation should validate before waiting");
    active.store(false, Ordering::Release);
    release_tx.send(()).expect("release database mutex");
    locker.join().expect("locker thread")?;
    assert!(matches!(
        mutation.join().expect("mutation thread"),
        Err(Error::VaultAuthenticationFailed)
    ));
    assert_eq!(Settings::new(database).trash_retention_days()?, 30);
    Ok(())
}
