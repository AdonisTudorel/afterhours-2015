// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Experimental police on the owned traffic population (fork experiment for
//! #55, not upstream behaviour).
//!
//! Patrol cars are the installed police traffic persona. The host decides when
//! a pursuit starts and ends; the delegated client drives every chasing car
//! with the native ChaseTargetGoal. Every threshold here is this fork's choice,
//! not a native value. `AFTERHOURS_COPS=0` turns police off;
//! `AFTERHOURS_COP_PROBE=0` keeps patrol cars on the plain traffic goal
//! instead of adding the native LookForLawbreakersGoal.
use super::{Car, Changes, Focus, distance_squared};
use nfs_content::traffic::Content;
use nfs_world::replication::{
    Initial, Update,
    players::{Players, ai},
    target_goal::{Chase, LookForLawbreakers},
    traffic, vehicle, wander,
};
use std::collections::BTreeMap;

/// A patrol car notices a player this close (metres)...
const SEE: f32 = 40.;
/// ...driving faster than this (m/s, about 100 km/h).
const SPEEDING: f32 = 28.;
/// A moving player this close while the patrol car's reported velocity jumps
/// by more than `JOLT` (m/s) between two ticks counts as hitting it. Passing
/// close without contact does not change the car's velocity.
const HIT: f32 = 7.;
const HIT_SPEED: f32 = 5.;
const JOLT: f32 = 3.;
/// Patrol cars this close to the player join a running pursuit.
const JOIN: f32 = 200.;
/// A chasing car this close keeps the player seen.
const SIGHT: f32 = 220.;
/// Unseen this long: escaped.
const ESCAPE_MS: u64 = 15_000;
/// Stopped this close to a chasing car this long: busted.
const BUSTED_MS: u64 = 6_000;
const BUSTED_DISTANCE: f32 = 12.;
const BUSTED_SPEED: f32 = 2.5;
/// Heat rises every minute of pursuit; it allows 1 + heat chasing cars.
const HEAT_MS: u64 = 60_000;
const MAX_HEAT: u64 = 5;
/// At most one reinforcement this often.
const DISPATCH_MS: u64 = 8_000;
/// No new pursuit this soon after one ends.
const COOLDOWN_MS: u64 = 20_000;
const LOG_MS: u64 = 1_000;
/// WanderGoal flags of official roaming cars (random offset only).
const WANDER: [bool; 3] = [false, false, true];

/// The world transaction a police decision writes into.
pub(super) struct Host<'a> {
    pub world: &'a mut Players,
    pub connection: u8,
    pub persona: u64,
    pub changes: &'a mut Changes,
}

impl Host<'_> {
    fn apply(&mut self, out: ai::GoalChanges) {
        self.changes.records.extend(out.records);
        self.changes.deleted.extend(out.deleted);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Patrol,
    Chasing,
}

#[derive(Clone, Debug)]
struct Cop {
    state: State,
    /// A reinforcement's player, chased from the tick after creation.
    pending: Option<u16>,
    /// Reported velocity at the previous tick, for hit detection.
    last_velocity: Option<[f32; 3]>,
    /// Profile indices of the car's siren and searchlight components.
    sirens: Vec<usize>,
    searchlights: Vec<usize>,
}

#[derive(Clone, Debug)]
struct Pursuit {
    target: u16,
    started: u64,
    last_seen: u64,
    stopped_since: Option<u64>,
    last_dispatch: u64,
    last_log: u64,
}

impl Pursuit {
    fn heat(&self, now: u64) -> u64 {
        (1 + now.saturating_sub(self.started) / HEAT_MS).min(MAX_HEAT)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Police {
    enabled: bool,
    probe: bool,
    personas: Vec<usize>,
    cops: BTreeMap<u64, Cop>,
    pursuit: Option<Pursuit>,
    quiet_until: u64,
}

fn switch(name: &str) -> bool {
    std::env::var(name).map_or(true, |v| v.trim() != "0")
}

fn length(v: [f32; 3]) -> f32 {
    v.iter().map(|c| c * c).sum::<f32>().sqrt()
}

impl Police {
    pub(super) fn new(content: &Content) -> Self {
        let personas: Vec<_> = content
            .personas
            .iter()
            .enumerate()
            .filter(|(_, p)| p.vehicle.to_ascii_lowercase().contains("police"))
            .map(|(i, _)| i)
            .collect();
        let enabled = switch("AFTERHOURS_COPS") && !personas.is_empty();
        let probe = switch("AFTERHOURS_COP_PROBE");
        tracing::info!(enabled, probe, personas = ?personas, "police experiment");
        Self {
            enabled,
            probe,
            personas,
            cops: BTreeMap::new(),
            pursuit: None,
            quiet_until: 0,
        }
    }

    pub(super) fn is_police(&self, persona: usize) -> bool {
        self.enabled && self.personas.contains(&persona)
    }

    /// Remember a new police car and where its lights are.
    pub(super) fn register(&mut self, key: u64, kinds: &[vehicle::Kind]) {
        let find = |want: fn(&vehicle::Kind) -> bool| {
            kinds
                .iter()
                .enumerate()
                .filter(|(_, k)| want(k))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        let cop = Cop {
            state: State::Patrol,
            pending: None,
            last_velocity: None,
            sirens: find(|k| matches!(k, vehicle::Kind::Siren)),
            searchlights: find(|k| matches!(k, vehicle::Kind::SearchLight)),
        };
        tracing::info!(
            cop = key,
            sirens = cop.sirens.len(),
            searchlights = cop.searchlights.len(),
            "police car spawned"
        );
        self.cops.insert(key, cop);
    }

    /// First commands of a new police car. False: give it the plain traffic goal.
    pub(super) fn start(
        &mut self,
        host: &mut Host<'_>,
        key: u64,
        watch: Option<u16>,
        chase: Option<u16>,
        road: &traffic::Initial,
    ) -> bool {
        // A reinforcement starts like any patrol car and switches to chasing on
        // the next tick: a car created directly with a chase command crashed
        // the game in the first live test (2026-11-11).
        if let Some(target) = chase
            && let Some(cop) = self.cops.get_mut(&key)
        {
            cop.pending = Some(target);
        }
        let Some(watch) = watch.or(chase).filter(|_| self.probe) else {
            return false;
        };
        let flag = key.is_multiple_of(2);
        let goals = vec![
            (Initial::TrafficGoal(road.clone()), Update::TrafficGoal),
            (
                Initial::LookForLawbreakersGoal(LookForLawbreakers {
                    allocation_serial: 0,
                    target: watch,
                    flag,
                }),
                Update::LookForLawbreakersGoal,
            ),
        ];
        match host
            .world
            .replace_ai_goals(host.connection, host.persona, key, goals, None)
        {
            Ok(out) => {
                host.apply(out);
                tracing::info!(
                    cop = key,
                    watch,
                    flag,
                    "police patrol with LookForLawbreakersGoal"
                );
                true
            }
            Err(error) => {
                tracing::warn!(cop = key, ?error, "police patrol probe refused");
                false
            }
        }
    }

    /// Switch one police car to chasing `target` (a player's participant).
    fn chase(&mut self, host: &mut Host<'_>, key: u64, target: u16) -> bool {
        let Some(cop) = self.cops.get_mut(&key) else {
            return false;
        };
        // The participant reference made a traffic police car chase in the
        // live test; a reference to the player's car coincided with a crash.
        let goal = (
            Initial::ChaseTargetGoal(Chase {
                allocation_serial: 0,
                target,
            }),
            Update::ChaseTargetGoal,
        );
        match host
            .world
            .replace_ai_goals(host.connection, host.persona, key, vec![goal], None)
        {
            Ok(out) => {
                host.apply(out);
                cop.state = State::Chasing;
                lights(host, key, cop, true);
                tracing::info!(cop = key, player = target, "police car chasing");
                true
            }
            Err(error) => {
                tracing::warn!(cop = key, ?error, "police chase refused");
                false
            }
        }
    }

    /// Return a chasing car to roaming with its lights off.
    fn release(&mut self, host: &mut Host<'_>, key: u64) {
        let Some(cop) = self.cops.get_mut(&key) else {
            return;
        };
        let goal = (
            Initial::WanderGoal(wander::Initial {
                allocation_serial: 0,
                flags: WANDER,
            }),
            Update::WanderGoal,
        );
        match host
            .world
            .replace_ai_goals(host.connection, host.persona, key, vec![goal], None)
        {
            Ok(out) => host.apply(out),
            Err(error) => tracing::warn!(cop = key, ?error, "police release refused"),
        }
        cop.state = State::Patrol;
        lights(host, key, cop, false);
    }

    fn end(&mut self, host: &mut Host<'_>, now: u64, reason: &str) {
        let Some(pursuit) = self.pursuit.take() else {
            return;
        };
        let chasing: Vec<_> = self
            .cops
            .iter()
            .filter(|(_, c)| c.state == State::Chasing)
            .map(|(&k, _)| k)
            .collect();
        for key in &chasing {
            self.release(host, *key);
        }
        self.quiet_until = now + COOLDOWN_MS;
        tracing::info!(
            player = pursuit.target,
            reason,
            seconds = now.saturating_sub(pursuit.started) / 1000,
            heat = pursuit.heat(now),
            released = chasing.len(),
            "police pursuit ended"
        );
    }

    /// A patrol car that sees a player speeding or hitting it.
    fn spot(
        &self,
        cars: &BTreeMap<u64, Car>,
        focus: &[Focus],
        jolts: &BTreeMap<u64, f32>,
    ) -> Option<(u64, u16, &'static str)> {
        for (&key, cop) in &self.cops {
            if cop.state != State::Patrol {
                continue;
            }
            let Some(car) = cars.get(&key) else {
                continue;
            };
            let jolt = jolts.get(&key).copied().unwrap_or(0.);
            for point in focus {
                let d2 = distance_squared(car.anchor, point.position);
                if d2 < HIT * HIT && jolt > JOLT && length(point.velocity) > HIT_SPEED {
                    return Some((key, point.id, "hit"));
                }
                if d2 < SEE * SEE && length(point.velocity) > SPEEDING {
                    return Some((key, point.id, "speeding"));
                }
            }
        }
        None
    }

    /// One host tick, after the population refreshed motion and retired cars.
    pub(super) fn tick(
        &mut self,
        host: &mut Host<'_>,
        cars: &BTreeMap<u64, Car>,
        focus: &[Focus],
        now: u64,
    ) {
        if !self.enabled {
            return;
        }
        self.cops.retain(|key, _| cars.contains_key(key));
        let mut jolts = BTreeMap::new();
        for (key, cop) in &mut self.cops {
            let Some(car) = cars.get(key) else {
                continue;
            };
            if let Some(last) = cop.last_velocity {
                jolts.insert(
                    *key,
                    length(std::array::from_fn(|i| car.velocity[i] - last[i])),
                );
            }
            cop.last_velocity = Some(car.velocity);
        }
        if self
            .pursuit
            .as_ref()
            .is_some_and(|p| !focus.iter().any(|f| f.id == p.target))
        {
            self.end(host, now, "player left free roam");
        }
        if self.pursuit.is_none()
            && now >= self.quiet_until
            && let Some((key, target, why)) = self.spot(cars, focus, &jolts)
        {
            tracing::info!(cop = key, player = target, why, "police pursuit started");
            self.pursuit = Some(Pursuit {
                target,
                started: now,
                last_seen: now,
                stopped_since: None,
                last_dispatch: now,
                last_log: 0,
            });
            self.chase(host, key, target);
        }
        let Some(mut pursuit) = self.pursuit.clone() else {
            return;
        };
        let Some(player) = focus.iter().find(|f| f.id == pursuit.target) else {
            return;
        };
        let pending: Vec<_> = self
            .cops
            .iter()
            .filter_map(|(&k, c)| c.pending.map(|t| (k, t)))
            .collect();
        for (key, target) in pending {
            if let Some(cop) = self.cops.get_mut(&key) {
                cop.pending = None;
            }
            if target == pursuit.target {
                self.chase(host, key, target);
            }
        }
        let joining: Vec<_> = self
            .cops
            .iter()
            .filter(|(k, c)| {
                c.state == State::Patrol
                    && cars.get(*k).is_some_and(|car| {
                        distance_squared(car.anchor, player.position) < JOIN * JOIN
                    })
            })
            .map(|(&k, _)| k)
            .collect();
        for key in joining {
            self.chase(host, key, pursuit.target);
        }
        let chasers: Vec<(u64, u32, u32)> = self
            .cops
            .iter()
            .filter(|(_, c)| c.state == State::Chasing)
            .filter_map(|(&k, _)| {
                let car = cars.get(&k)?;
                let distance = distance_squared(car.anchor, player.position).sqrt();
                Some((k, distance as u32, (length(car.velocity) * 3.6) as u32))
            })
            .collect();
        let nearest = chasers
            .iter()
            .map(|c| c.1 as f32)
            .fold(f32::INFINITY, f32::min);
        let speed = length(player.velocity);
        if nearest < SIGHT {
            pursuit.last_seen = now;
        }
        if speed < BUSTED_SPEED && nearest < BUSTED_DISTANCE {
            let since = *pursuit.stopped_since.get_or_insert(now);
            if now.saturating_sub(since) >= BUSTED_MS {
                self.pursuit = Some(pursuit);
                self.end(host, now, "busted");
                return;
            }
        } else {
            pursuit.stopped_since = None;
        }
        if now.saturating_sub(pursuit.last_seen) >= ESCAPE_MS {
            self.pursuit = Some(pursuit);
            self.end(host, now, "escaped");
            return;
        }
        if now.saturating_sub(pursuit.last_log) >= LOG_MS {
            pursuit.last_log = now;
            tracing::info!(
                player = pursuit.target,
                heat = pursuit.heat(now),
                seconds = now.saturating_sub(pursuit.started) / 1000,
                player_kmh = (speed * 3.6) as u32,
                chasers = ?chasers,
                "police pursuit (cop, metres, km/h)"
            );
        }
        self.pursuit = Some(pursuit);
    }

    /// The player a reinforcement should be created for, at most once per
    /// dispatch interval while fewer than 1 + heat cars chase.
    pub(super) fn dispatch(&mut self, now: u64) -> Option<(u16, usize)> {
        if !self.enabled {
            return None;
        }
        let chasing = self
            .cops
            .values()
            .filter(|c| c.state == State::Chasing)
            .count() as u64;
        let pursuit = self.pursuit.as_mut()?;
        if chasing > pursuit.heat(now) || now.saturating_sub(pursuit.last_dispatch) < DISPATCH_MS {
            return None;
        }
        pursuit.last_dispatch = now;
        Some((pursuit.target, *self.personas.first()?))
    }
}

fn lights(host: &mut Host<'_>, key: u64, cop: &Cop, on: bool) {
    let fields: Vec<_> = cop
        .sirens
        .iter()
        .map(|&i| {
            (
                i,
                vehicle::Update::Siren {
                    flags: [Some(on); 2],
                    trigger: false,
                },
            )
        })
        .chain(
            cop.searchlights
                .iter()
                .map(|&i| (i, vehicle::Update::SearchLight(Some(on)))),
        )
        .collect();
    if fields.is_empty() {
        return;
    }
    match host
        .world
        .set_ai_vehicle_fields(host.connection, host.persona, key, &fields)
    {
        Ok(Some(record)) => host.changes.records.push(record),
        Ok(None) => {}
        Err(error) => tracing::warn!(cop = key, ?error, "police lights not changed"),
    }
}
