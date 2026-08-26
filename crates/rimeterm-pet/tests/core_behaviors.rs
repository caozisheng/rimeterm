use chrono::{Duration, TimeZone, Utc};
use rimeterm_pet::state::{Character, LifeStage, PetState};
use rimeterm_pet::{actions, engine};

fn at(hour: u32, minute: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 26, hour, minute, 0)
        .single()
        .expect("valid test time")
}

#[test]
fn meal_increases_hunger_without_exceeding_four() {
    let now = at(12, 0);
    let mut pet = PetState::new_egg(now);
    pet.stage = LifeStage::Child;
    pet.character = Character::Marutchi;
    pet.hunger = 4;

    actions::feed_meal(&mut pet).expect("awake healthy pet can eat");

    assert_eq!(pet.hunger, 4);
}

#[test]
fn egg_hatches_after_five_minutes() {
    let now = at(12, 0);
    let mut pet = PetState::new_egg(now);

    engine::tick(&mut pet, now + Duration::minutes(5));

    assert_eq!(pet.stage, LifeStage::Baby);
}

#[test]
fn sleeping_pet_does_not_decay() {
    let now = at(23, 0);
    let mut pet = PetState::new_egg(now);
    pet.stage = LifeStage::Child;
    pet.character = Character::Marutchi;
    pet.hunger = 4;
    pet.happiness = 4;
    pet.is_sleeping = true;

    engine::tick(&mut pet, now + Duration::minutes(120));

    assert_eq!((pet.hunger, pet.happiness), (4, 4));
}

#[test]
fn dead_pet_rejects_actions() {
    let now = at(12, 0);
    let mut pet = PetState::new_egg(now);
    pet.is_alive = false;
    pet.stage = LifeStage::Dead;

    let error = actions::feed_meal(&mut pet).expect_err("dead pets cannot eat");

    assert_eq!(error, actions::ActionError::PetIsDead);
}
