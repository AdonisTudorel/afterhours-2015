// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Session-owned ambient traffic with installed roads and native population rules.
use nfs_content::traffic::Content;
use nfs_world::{
    content::bindings::{Bindings, Level, name_key},
    participants::HostRpc,
    replication::{
        Error, Record,
        entity::creation::{Asset, Catalog, Content as EntityContent, Parent, Prefix},
        players::{Players, ai},
        traffic, vehicle,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

mod police;
mod policy;
pub use policy::{Focus, INNER, OUTER};

#[cfg(test)]
pub(crate) mod tests;

/// Traffic shares the world's AI budget but leaves room for every rival car
/// a player can choose, even at the largest traffic amount.
pub const MAX_CARS: usize = ai::MAX_AI - crate::ai_settings::RIVAL_CAR_RESERVE;
/// Disjoint host identity ranges for ambient traffic and roaming rivals.
/// These are server bookkeeping keys, never client object IDs.
pub(crate) const RIVAL_KEY_START: u64 = 1 << 40;

pub struct Input<'a> {
    pub now: u64,
    pub focus: &'a [Focus],
    pub random: [u32; 2],
}

#[derive(Clone, Debug, PartialEq)]
struct Timer {
    elapsed: f32,
    interval: f32,
}

#[derive(Clone, Debug)]
struct Car {
    anchor: [f32; 3],
    velocity: [f32; 3],
    forward: [f32; 3],
    road: usize,
    born: u64,
}

#[derive(Clone, Debug)]
pub struct Population {
    bindings: Bindings,
    catalogs: BTreeMap<u32, Level>,
    authority: vehicle::authority::Registry,
    policy: Arc<policy::Policy>,
    cars: BTreeMap<u64, Car>,
    road_counts: BTreeMap<usize, u32>,
    timers: BTreeMap<u16, Timer>,
    persona_cursor: usize,
    next: u64,
    clock: u64,
    owner: Option<(u8, u64)>,
    /// Player-chosen amount, percent of the original traffic (100 default).
    traffic_percent: u16,
    /// Fork experiment: patrol cars and pursuits.
    police: police::Police,
}

#[derive(Clone, Debug, Default)]
pub struct Changes {
    pub leave: Vec<HostRpc>,
    pub records: Vec<Record>,
    pub calls: Vec<HostRpc>,
    pub deleted: Vec<u16>,
    pub spawned: Option<ai::Ids>,
}

#[derive(Clone, Debug)]
struct Placement {
    spline: usize,
    persona: usize,
    born: u64,
    pose: vehicle::initialize::Pose,
    /// The player whose focus created the car (police watch target).
    watch: Option<u16>,
    /// A police reinforcement created to chase this player's participant.
    chase: Option<u16>,
}

impl Population {
    pub fn new(bindings: Bindings, content: &Content) -> Result<Self, Error> {
        let gameplay = bindings
            .root_child(content.managers.scene)
            .map_err(|_| Error::Shape)?;
        if gameplay.id >= 2048 {
            return Err(Error::Bound);
        }
        for definition in content.vehicles.values() {
            let key = name_key(definition.bundle.as_bytes()).map_err(|_| Error::Shape)?;
            let bundle = bindings
                .root_child(key)
                .map_err(|_| Error::Shape)?
                .bundle_id;
            asset_bundle(bundle)?;
        }
        let levels = bindings.hierarchy();
        let mut catalogs = BTreeMap::new();
        for road in &content.splines {
            let mut matches = levels
                .iter()
                .filter(|level| level.content_key == road.scene);
            let level = *matches.next().ok_or(Error::UnknownObject)?;
            if matches.next().is_some() || level.id >= 2048 {
                return Err(Error::Shape);
            }
            catalogs.insert(road.scene, level);
        }
        Ok(Self {
            bindings,
            catalogs,
            authority: vehicle::authority::Registry::default(),
            policy: Arc::new(policy::Policy::new(content)?),
            cars: BTreeMap::new(),
            road_counts: BTreeMap::new(),
            timers: BTreeMap::new(),
            persona_cursor: 0,
            next: 1,
            clock: 0,
            owner: None,
            traffic_percent: 100,
            police: police::Police::new(content),
        })
    }

    /// Scale the original traffic by `percent` (0 turns traffic off).
    pub fn with_traffic_percent(mut self, percent: u16) -> Self {
        self.set_traffic_percent(percent);
        self
    }

    /// Change the amount while the session runs; the next spawn decision
    /// uses it. Cars above a lower amount retire as they leave the retain
    /// range, never in view.
    pub fn set_traffic_percent(&mut self, percent: u16) {
        self.traffic_percent = percent;
    }

    pub fn traffic_percent(&self) -> u16 {
        self.traffic_percent
    }

    pub fn count(&self) -> usize {
        self.cars.len()
    }

    pub fn delegated(&self, world: &Players, connection: u8, persona: u64) -> BTreeMap<u16, u64> {
        if self.owner != Some((connection, persona)) {
            return BTreeMap::new();
        }
        self.cars
            .keys()
            .filter_map(|&key| {
                world
                    .ai_ids(connection, persona, key)
                    .map(|ids| (ids.vehicle, key))
            })
            .collect()
    }

    /// Every car's spawn key, world position and forward axis, for the
    /// launcher's map: the client's latest report, or the pose the host kept
    /// at its last advance when the report does not decode.
    pub fn poses(
        &self,
        world: &Players,
        connection: u8,
        persona: u64,
    ) -> Vec<(u64, [[f32; 3]; 2])> {
        if self.owner != Some((connection, persona)) {
            return Vec::new();
        }
        self.cars
            .iter()
            .map(|(&key, car)| {
                let reported = world
                    .ai_vehicle_physics(connection, persona, key)
                    .and_then(|physics| physics.decode([0.; 3]))
                    .and_then(|motion| {
                        let basis = vehicle::orientation::basis_from_quaternion(motion.quaternion)?;
                        Ok([motion.position, basis[2]])
                    });
                (key, reported.unwrap_or([car.anchor, car.forward]))
            })
            .collect()
    }

    /// The caller commits this transaction only after reserving its outbound
    /// sections. Regressing time, incomplete content and late manager errors
    /// preserve both the world namespace and population counters.
    pub fn advance(
        &mut self,
        players: &mut Players,
        connection: u8,
        persona: u64,
        content: &Content,
        input: Input<'_>,
    ) -> Result<Changes, Error> {
        let Input { now, focus, random } = input;
        if now < self.clock || focus.len() > 128 {
            return Err(Error::Bound);
        }
        let mut ids = BTreeSet::new();
        for point in focus {
            point.validate()?;
            if !ids.insert(point.id) {
                return Err(Error::DuplicateObject);
            }
        }
        if self
            .owner
            .is_some_and(|owner| owner != (connection, persona))
        {
            return Err(Error::UnknownObject);
        }
        let mut next = self.clone();
        let mut world = players.clone();
        let mut changes = Changes::default();
        for (&key, car) in &mut next.cars {
            let motion = world
                .ai_vehicle_physics(connection, persona, key)?
                .decode([0.; 3])?;
            car.anchor = motion.position;
            car.velocity = motion.velocity;
            car.forward = vehicle::orientation::basis_from_quaternion(motion.quaternion)?[2];
        }
        let retire: Vec<_> = next
            .cars
            .iter()
            .filter(|(_, car)| {
                focus.is_empty()
                    || (now - car.born > 2000 && !focus.iter().any(|p| p.retains(car.anchor)))
            })
            .map(|(&key, _)| key)
            .collect();
        for key in retire {
            let removed = world.remove_ai_vehicle(connection, persona, key)?;
            changes.records.extend(removed.records);
            changes
                .leave
                .extend(removed.leave.into_iter().map(HostRpc::Participant));
            changes
                .leave
                .extend(removed.untracked.into_iter().map(HostRpc::WorldActivation));
            changes.deleted.extend(removed.deleted);
            let car = next.cars.remove(&key).ok_or(Error::UnknownObject)?;
            let count = next
                .road_counts
                .get_mut(&car.road)
                .ok_or(Error::UnknownObject)?;
            *count = count.checked_sub(1).ok_or(Error::Shape)?;
            next.authority.remove_group(key);
        }
        next.police.tick(
            &mut police::Host {
                world: &mut world,
                connection,
                persona,
                changes: &mut changes,
            },
            &next.cars,
            focus,
            now,
        );
        // A police reinforcement is created like traffic on a road near the
        // chased player. Its failure never blocks ambient traffic.
        if next.cars.len() < MAX_CARS
            && world.ai_count() < ai::MAX_AI
            && let Some((target, cop)) = next.police.dispatch(now)
            && let Some(point) = focus.iter().find(|p| p.id == target)
        {
            let query = next.policy.query(point, next.traffic_percent.max(100));
            if let Some(candidate) =
                next.policy
                    .select(&query, point, focus, &next.cars, &next.road_counts, random)
            {
                let placement = Placement {
                    spline: candidate.road,
                    persona: cop,
                    born: now,
                    pose: vehicle::initialize::Pose {
                        position: candidate.position,
                        basis: candidate.basis,
                        origin: [0.; 3],
                    },
                    watch: None,
                    chase: Some(target),
                };
                let saved = (next.clone(), world.clone(), changes.clone());
                if let Err(error) = next.spawn(
                    &mut world,
                    connection,
                    persona,
                    content,
                    placement,
                    &mut changes,
                ) {
                    tracing::warn!(?error, "police reinforcement not created");
                    (next, world, changes) = saved;
                }
            }
        }
        next.timers.retain(|id, _| ids.contains(id));
        for point in focus {
            let timer = next
                .timers
                .entry(point.id)
                .and_modify(|t| t.elapsed += (now - self.clock) as f32 * 0.001)
                .or_insert(Timer {
                    elapsed: 0.,
                    interval: 1.,
                });
            if changes.spawned.is_some()
                || next.cars.len() >= MAX_CARS
                || world.ai_count() >= ai::MAX_AI
                || timer.elapsed <= timer.interval
            {
                continue;
            }
            timer.elapsed -= timer.interval;
            let query = next.policy.query(point, next.traffic_percent);
            if query.target == 0 {
                // Traffic turned off by the player.
                continue;
            }
            let nearby = next.policy.nearby(point, &next.cars);
            timer.interval = policy::interval(nearby, query.target, next.traffic_percent);
            if nearby >= query.target {
                continue;
            }
            if let Some(candidate) =
                next.policy
                    .select(&query, point, focus, &next.cars, &next.road_counts, random)
                && let Some((selected, cursor)) =
                    next.policy.persona(candidate.road, next.persona_cursor)
            {
                let placement = Placement {
                    spline: candidate.road,
                    persona: selected,
                    born: now,
                    pose: vehicle::initialize::Pose {
                        position: candidate.position,
                        basis: candidate.basis,
                        origin: [0.; 3],
                    },
                    watch: Some(point.id),
                    chase: None,
                };
                next.spawn(
                    &mut world,
                    connection,
                    persona,
                    content,
                    placement,
                    &mut changes,
                )?;
                next.persona_cursor = cursor;
            }
        }
        next.clock = now;
        next.owner = Some((connection, persona));
        *self = next;
        *players = world;
        Ok(changes)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &mut self,
        world: &mut Players,
        connection: u8,
        persona: u64,
        content: &Content,
        placement: Placement,
        changes: &mut Changes,
    ) -> Result<(), Error> {
        let selected = content
            .personas
            .get(placement.persona)
            .ok_or(Error::Shape)?;
        let definition = content
            .vehicles
            .get(&selected.vehicle)
            .ok_or(Error::Shape)?;
        let content_key = name_key(definition.bundle.as_bytes()).map_err(|_| Error::Shape)?;
        let registration = self
            .bindings
            .root_child(content_key)
            .map_err(|_| Error::Shape)?;
        let parent = world
            .objects()
            .scene(content_key)
            .ok_or(Error::UnknownObject)?;
        let blueprint = world
            .objects()
            .scene(content.managers.scene)
            .ok_or(Error::UnknownObject)?;
        let key = self.next;
        if key >= RIVAL_KEY_START {
            return Err(Error::Bound);
        }
        let components = definition
            .profile
            .kinds()
            .iter()
            .enumerate()
            .filter(|(_, kind)| matches!(kind, vehicle::Kind::FourBit))
            .map(|(i, _)| {
                key.checked_mul(512)
                    .and_then(|v| v.checked_add(i as u64 + 1))
                    .ok_or(Error::Bound)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let body = super::construct(
            definition,
            super::Request {
                pose: placement.pose,
                speed: 0.,
                authority_group: key,
                authority_components: &components,
            },
            &mut self.authority,
        )?;
        let asset = Asset {
            bundle: asset_bundle(registration.bundle_id)?,
            type_id: definition.asset_type,
            local_index: definition.asset_index,
        };
        let entity_content = EntityContent::new(
            vec![Catalog::new(asset.bundle, &definition.catalog)?],
            vec![],
        )?
        .with_vehicles(vec![(asset, definition.profile.clone())])?;
        let spawned = world
            .spawn_ai_vehicle(
                connection,
                persona,
                ai::Request {
                    key,
                    name: selected.name.as_bytes().to_vec(),
                    flags: [true, true],
                    words: [selected.index, u16::MAX],
                    mode: 0,
                },
                vehicle::EntityCreation {
                    prefix: Prefix {
                        parent: Some(Parent {
                            reference: Some(parent),
                        }),
                        blueprint,
                        sub_id: 2,
                        owner: None,
                        asset,
                    },
                    body,
                },
                &entity_content,
            )?
            .ok_or(Error::DuplicateObject)?;
        changes.records.extend(spawned.records);
        let bound = world.bind_ai_managers(connection, persona, key, content.managers)?;
        changes.calls.extend(bound.add.map(HostRpc::Participant));
        changes.calls.extend(bound.bind.map(HostRpc::Actor));
        changes.calls.extend(bound.enter.map(HostRpc::Participant));
        changes
            .calls
            .extend(bound.tracked.map(HostRpc::WorldActivation));
        let spline = content.splines.get(placement.spline).ok_or(Error::Shape)?;
        let level = self
            .catalogs
            .get(&spline.scene)
            .ok_or(Error::UnknownObject)?;
        world
            .objects()
            .scene(spline.scene)
            .ok_or(Error::UnknownObject)?;
        let road = traffic::Initial {
            asset: Asset {
                bundle: level.id,
                type_id: content.spline_class,
                local_index: spline.index,
            },
            catalog: Catalog::new(level.id, &[(content.spline_class, spline.catalog_count)])?,
            allocation_serial: 0,
            asset_mode: 2,
        };
        let is_cop = self.police.is_police(placement.persona);
        if is_cop {
            self.police.register(key, definition.profile.kinds());
        }
        let commanded = is_cop
            && self.police.start(
                &mut police::Host {
                    world: &mut *world,
                    connection,
                    persona,
                    changes: &mut *changes,
                },
                key,
                placement.watch,
                placement.chase,
                &road,
            );
        if !commanded {
            let (goal, assigned) = world.start_ai_traffic_goal(connection, persona, key, road)?;
            changes.records.extend([goal, assigned]);
        }
        changes.spawned = Some(spawned.ids);
        self.next = key.checked_add(1).ok_or(Error::Bound)?;
        self.cars.insert(
            key,
            Car {
                anchor: placement.pose.position,
                velocity: [0.; 3],
                forward: placement.pose.basis[2],
                road: placement.spline,
                born: placement.born,
            },
        );
        *self.road_counts.entry(placement.spline).or_default() += 1;
        Ok(())
    }
}

fn asset_bundle(stream: u16) -> Result<u16, Error> {
    let asset = stream.checked_sub(3).ok_or(Error::Shape)?;
    if asset >= 2048 || asset == 2003 {
        return Err(Error::Bound);
    }
    Ok(asset)
}
pub(super) fn distance_squared(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum()
}
fn horizontal_distance_squared(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)
}
pub(super) fn road_basis(a: [f32; 3], b: [f32; 3]) -> Option<[[f32; 3]; 3]> {
    let mut forward = std::array::from_fn::<_, 3, _>(|i| b[i] - a[i]);
    forward[1] = 0.;
    let length = forward.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !length.is_finite() || length < 0.01 {
        return None;
    }
    forward = forward.map(|v| v / length);
    Some([[forward[2], 0., -forward[0]], [0., 1., 0.], forward])
}
