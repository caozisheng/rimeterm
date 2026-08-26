use chrono::{Duration, TimeZone, Utc};
use rimeterm_pet::persistence::{PetStore, StoreMode};
use rimeterm_pet::state::{LifeStage, PetState};

fn at(minute: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 26, 12, minute, 0)
        .single()
        .expect("valid test time")
}

fn paths(name: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let state = directory.path().join(format!("{name}.json"));
    let lock = directory.path().join(format!("{name}.lock"));
    (directory, state, lock)
}

#[test]
fn first_store_is_owner_and_second_store_is_spectator() {
    let (_directory, state, lock) = paths("ownership");
    let owner = PetStore::open(&state, &lock, at(0)).expect("open owner");
    let spectator = PetStore::open(&state, &lock, at(0)).expect("open spectator");
    assert_eq!(
        (owner.mode(), spectator.mode()),
        (StoreMode::Owner, StoreMode::Spectator)
    );
}

#[test]
fn owner_save_round_trips_pet_state() {
    let (_directory, state, lock) = paths("round-trip");
    let mut owner = PetStore::open(&state, &lock, at(0)).expect("open owner");
    owner.state_mut().hunger = 3;
    owner.save().expect("save state");
    drop(owner);
    let reopened = PetStore::open(&state, &lock, at(0)).expect("reopen state");
    assert_eq!(reopened.state().hunger, 3);
}

#[test]
fn load_applies_elapsed_time_catch_up() {
    let (_directory, state, lock) = paths("catch-up");
    let mut owner = PetStore::open(&state, &lock, at(0)).expect("open owner");
    let start = at(0);
    *owner.state_mut() = PetState::new_egg(start);
    owner.save().expect("save egg");
    drop(owner);
    let reopened =
        PetStore::open(&state, &lock, start + Duration::minutes(5)).expect("reopen state");
    assert_eq!(reopened.state().stage, LifeStage::Baby);
}

#[test]
fn corrupt_state_is_backed_up_before_fresh_egg() {
    let (directory, state, lock) = paths("corrupt");
    std::fs::write(&state, "{not-json").expect("write corrupt fixture");
    let store = PetStore::open(&state, &lock, at(0)).expect("recover corrupt state");
    let backups = std::fs::read_dir(directory.path())
        .expect("list fixture directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt-"))
        .count();
    assert_eq!((store.state().stage.clone(), backups), (LifeStage::Egg, 1));
}

#[test]
fn spectator_reload_observes_owner_save() {
    let (_directory, state, lock) = paths("spectator-refresh");
    let mut owner = PetStore::open(&state, &lock, at(0)).expect("open owner");
    let mut spectator = PetStore::open(&state, &lock, at(0)).expect("open spectator");
    owner.state_mut().hunger = 4;
    owner.save().expect("save owner state");
    spectator.reload(at(0)).expect("reload spectator state");
    assert_eq!(spectator.state().hunger, 4);
}

#[test]
fn stale_lock_is_reclaimed_by_new_owner() {
    let (_directory, state, lock) = paths("stale-lock");
    std::fs::write(&lock, "4294967294\n").expect("write stale lock");
    let store = PetStore::open(&state, &lock, at(0)).expect("reclaim stale lock");
    assert_eq!(store.mode(), StoreMode::Owner);
}
