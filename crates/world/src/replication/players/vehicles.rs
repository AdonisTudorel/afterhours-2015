// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::*;

impl Players {
    /// Set or clear only the owned car's burnout tendency override. None resets
    /// the native mask-8 pair to (0, false); finite values are not clamped.
    pub fn set_vehicle_burnout_tendency(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        value: Option<f32>,
    ) -> Result<Option<Record>, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        self.change_vehicle_burnout_tendency(id, value)
    }

    /// Resolve an AI car through the host-issued lifetime key before applying
    /// an event's burnout override. This grants no inventory authority.
    pub fn set_ai_vehicle_burnout_tendency(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        value: Option<f32>,
    ) -> Result<Option<Record>, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        self.change_vehicle_burnout_tendency(id, value)
    }

    fn change_vehicle_burnout_tendency(
        &mut self,
        id: u16,
        value: Option<f32>,
    ) -> Result<Option<Record>, Error> {
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(Error::Bound);
        }
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        if fields
            .iter()
            .filter(|f| matches!(f, Some(vehicle::Update::Chassis(_))))
            .count()
            != 1
        {
            return Err(Error::Shape);
        }
        let updates = fields
            .iter()
            .map(|field| {
                matches!(field, Some(vehicle::Update::Chassis(_))).then(|| {
                    vehicle::Update::Chassis(Box::new(vehicle::ChassisUpdate {
                        physics: None,
                        flag: None,
                        custom: None,
                        pair: Some((value.unwrap_or(0.).to_bits(), value.is_some())),
                    }))
                })
            })
            .collect();
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }

    /// Publish the owned nitrous runtime's mode and/or level (0–255) on the car's
    /// nitrous field. `None` leaves that part of the field unchanged.
    pub fn set_vehicle_nitrous(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        mode: Option<u8>,
        level: Option<u8>,
    ) -> Result<Option<Record>, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        self.change_vehicle_nitrous(id, mode, level)
    }

    fn change_vehicle_nitrous(
        &mut self,
        id: u16,
        mode: Option<u8>,
        level: Option<u8>,
    ) -> Result<Option<Record>, Error> {
        if mode.is_some_and(|m| m > 3) {
            return Err(Error::Bound);
        }
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        if fields
            .iter()
            .filter(|f| matches!(f, Some(vehicle::Update::Index { .. })))
            .count()
            != 1
        {
            return Err(Error::Shape);
        }
        let updates = fields
            .iter()
            .map(|field| {
                matches!(field, Some(vehicle::Update::Index { .. })).then_some(
                    vehicle::Update::Index {
                        index: mode,
                        value: level,
                    },
                )
            })
            .collect();
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }
    /// Acknowledge a driver's nitrous transition without replacing its fuel,
    /// equipment, motion or binding. Identical repeats produce no new delta.
    pub fn set_vehicle_nitrous_mode(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        active: bool,
    ) -> Result<Option<Record>, Error> {
        self.set_vehicle_nitrous(connection, persona, id, Some(u8::from(active)), None)
    }

    /// Acknowledge a delegated AI driver's transition through its host-issued
    /// lifetime key. This does not grant inventory or equipment authority.
    pub fn set_ai_vehicle_nitrous_mode(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        active: bool,
    ) -> Result<Option<Record>, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        self.change_vehicle_nitrous(id, Some(u8::from(active)), None)
    }

    /// Change only the horn state of an owned car.
    pub fn set_vehicle_horn(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        sounding: bool,
    ) -> Result<Option<Record>, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        self.change_vehicle_horn(id, sounding)
    }

    /// Change only the horn state of the AI car identified by this lifetime key.
    pub fn set_ai_vehicle_horn(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        sounding: bool,
    ) -> Result<Option<Record>, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        self.change_vehicle_horn(id, sounding)
    }

    /// Change chosen component fields of the AI car identified by this lifetime
    /// key (fork experiment: police lights). Indices follow the car's installed
    /// profile; every other component stays unchanged.
    pub fn set_ai_vehicle_fields(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        changes: &[(usize, vehicle::Update)],
    ) -> Result<Option<Record>, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        let mut updates = vec![None; fields.len()];
        for (index, update) in changes {
            *updates.get_mut(*index).ok_or(Error::Shape)? = Some(update.clone());
        }
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }

    fn change_vehicle_horn(&mut self, id: u16, sounding: bool) -> Result<Option<Record>, Error> {
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        if fields
            .iter()
            .filter(|f| matches!(f, Some(vehicle::Update::Chassis(_))))
            .count()
            != 1
        {
            return Err(Error::Shape);
        }
        let updates = fields
            .iter()
            .map(|field| {
                matches!(field, Some(vehicle::Update::Chassis(_))).then(|| {
                    vehicle::Update::Chassis(Box::new(vehicle::ChassisUpdate {
                        physics: None,
                        flag: Some(sounding),
                        custom: None,
                        pair: None,
                    }))
                })
            })
            .collect();
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }

    /// Change one wheel's burst flag after checking inventory ownership.
    pub fn set_vehicle_tire_burst(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        component: usize,
        burst: bool,
    ) -> Result<Option<Record>, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        self.change_vehicle_tire_burst(id, component, burst)
    }

    /// Driving authority covers wheel reports without granting inventory access.
    pub fn set_ai_vehicle_tire_burst(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        component: usize,
        burst: bool,
    ) -> Result<Option<Record>, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        self.change_vehicle_tire_burst(id, component, burst)
    }

    fn change_vehicle_tire_burst(
        &mut self,
        id: u16,
        component: usize,
        burst: bool,
    ) -> Result<Option<Record>, Error> {
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        if !matches!(fields.get(component), Some(Some(vehicle::Update::Wheel(_)))) {
            return Err(Error::TypeMismatch);
        }
        let mut updates = vec![None; fields.len()];
        updates[component] = Some(vehicle::Update::Wheel(Some(burst)));
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }

    /// Publish only inventory-derived equipment fields, validating the complete
    /// delta before changing the owned object. Creation and motion are forbidden.
    pub fn set_vehicle_equipment(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        body: vehicle::Body,
    ) -> Result<Option<Record>, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        if body.creation.is_some() || body.updates.len() != fields.len() {
            return Err(Error::Shape);
        }
        let mut chassis = 0;
        for update in body.updates.iter().flatten() {
            match update {
                vehicle::Update::Chassis(c)
                    if c.physics.is_none()
                        && c.flag.is_none()
                        && c.custom.is_some()
                        && c.pair.is_none() =>
                {
                    chassis += 1
                }
                vehicle::Update::Tuning(Some(_))
                | vehicle::Update::Appearance(_)
                | vehicle::Update::Mesh(Some(_)) => (),
                _ => return Err(Error::Shape),
            }
        }
        if chassis != 1 {
            return Err(Error::Shape);
        }
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: body.updates,
            },
        )
    }
    /// Resolve a live car through this exact owned player's participant.
    pub fn player_vehicle_item(
        &self,
        connection: u8,
        persona: u64,
        player: u16,
        vehicle: u16,
    ) -> Option<(u16, u64)> {
        if !self.owns(connection, persona, player) {
            return None;
        }
        let participant = *self.participants.get(&player)?;
        self.vehicles.iter().find_map(|(&(owner, item), &ghost)| {
            (owner == participant && ghost == vehicle).then_some((participant, item))
        })
    }

    /// Publish only the owned car's chassis customization. Motion, health,
    /// authority, identity and every other component remain unchanged.
    pub fn set_vehicle_customization(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        custom: vehicle::Customization,
    ) -> Result<Option<Record>, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        if fields
            .iter()
            .filter(|f| matches!(f, Some(vehicle::Update::Chassis(_))))
            .count()
            != 1
        {
            return Err(Error::Shape);
        }
        let updates = fields
            .iter()
            .map(|field| {
                if matches!(field, Some(vehicle::Update::Chassis(_))) {
                    Some(vehicle::Update::Chassis(Box::new(vehicle::ChassisUpdate {
                        physics: None,
                        flag: None,
                        custom: Some(custom.clone()),
                        pair: None,
                    })))
                } else {
                    None
                }
            })
            .collect();
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }

    /// Current chassis state, only for a car owned by this connection/persona.
    pub fn vehicle_physics(
        &self,
        connection: u8,
        persona: u64,
        id: u16,
    ) -> Result<vehicle::Physics, Error> {
        if !self.owned_vehicles(connection, persona).contains(&id) {
            return Err(Error::UnknownObject);
        }
        self.physics_of(id)
    }

    fn physics_of(&self, id: u16) -> Result<vehicle::Physics, Error> {
        let Some(state::Object {
            current: Update::Vehicle { fields, .. },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        let mut matches = fields.iter().filter_map(|field| match field {
            Some(vehicle::Update::Chassis(c)) => c.physics.as_ref(),
            _ => None,
        });
        let physics = matches.next().ok_or(Error::Shape)?.clone();
        if matches.next().is_some() {
            return Err(Error::Shape);
        }
        Ok(physics)
    }
    /// Change only the chassis physics field. Other component state and the
    /// car's asset/participant binding are retained by the world model.
    pub fn set_vehicle_physics(
        &mut self,
        connection: u8,
        persona: u64,
        id: u16,
        physics: vehicle::Physics,
    ) -> Result<Option<Record>, Error> {
        self.vehicle_physics(connection, persona, id)?;
        self.change_physics(id, physics)
    }

    /// AI state is authorized by the host's spawn key and session. Delegating
    /// simulation is a separate host policy, not inventory ownership.
    pub fn ai_vehicle_physics(
        &self,
        connection: u8,
        persona: u64,
        key: u64,
    ) -> Result<vehicle::Physics, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        self.physics_of(id)
    }

    /// Change AI physics only through the session's host-issued spawn key.
    pub fn set_ai_vehicle_physics(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        physics: vehicle::Physics,
    ) -> Result<Option<Record>, Error> {
        let id = self
            .ai_ids(connection, persona, key)
            .ok_or(Error::UnknownObject)?
            .vehicle;
        self.physics_of(id)?;
        self.change_physics(id, physics)
    }

    fn change_physics(
        &mut self,
        id: u16,
        physics: vehicle::Physics,
    ) -> Result<Option<Record>, Error> {
        let Some(state::Object {
            current: Update::Vehicle { binding, fields },
            ..
        }) = self.objects.get(id)
        else {
            return Err(Error::TypeMismatch);
        };
        let updates = fields
            .iter()
            .map(|f| {
                if matches!(f, Some(vehicle::Update::Chassis(_))) {
                    Some(vehicle::Update::Chassis(Box::new(vehicle::ChassisUpdate {
                        physics: Some(physics.clone()),
                        flag: None,
                        custom: None,
                        pair: None,
                    })))
                } else {
                    None
                }
            })
            .collect();
        self.objects.change(
            id,
            Update::Vehicle {
                binding: binding.clone(),
                fields: updates,
            },
        )
    }

    pub(super) fn participant_has_vehicle(&self, participant: u16, vehicle: u16) -> bool {
        self.vehicles
            .iter()
            .any(|(&(p, _), &id)| p == participant && id == vehicle)
            || self.temporary_vehicles.get(&participant) == Some(&vehicle)
    }
    /// Simulation ownership covers both an inventory car and a session-only car.
    /// This check grants no item, equipment or purchase authority.
    pub fn owns_participant_vehicle(
        &self,
        connection: u8,
        persona: u64,
        participant: u16,
        vehicle: u16,
    ) -> bool {
        self.owns_participant(connection, persona, participant)
            && self.participant_has_vehicle(participant, vehicle)
    }
    /// Vehicles this connection's client simulates, including temporary cars.
    /// Temporary and display cars carry simulation ownership but no item identity;
    /// display cars remain excluded from participant driving assignments.
    pub fn owned_vehicles(&self, connection: u8, persona: u64) -> std::collections::BTreeSet<u16> {
        self.vehicles
            .iter()
            .chain(self.staged_vehicles.iter())
            .filter(|((participant, _), _)| {
                self.owns_participant(connection, persona, *participant)
            })
            .map(|(_, id)| *id)
            .chain(
                self.temporary_vehicles
                    .iter()
                    .filter(|(participant, _)| {
                        self.owns_participant(connection, persona, **participant)
                    })
                    .map(|(_, id)| *id),
            )
            .chain(
                self.preview_vehicles
                    .iter()
                    .filter(|((participant, _), _)| {
                        self.owns_participant(connection, persona, *participant)
                    })
                    .map(|(_, id)| *id),
            )
            .collect()
    }
    /// The item of a live car owned through this connection and persona.
    pub fn vehicle_item_of(&self, connection: u8, persona: u64, vehicle: u16) -> Option<u64> {
        self.vehicles
            .iter()
            .find(|((participant, _), ghost)| {
                **ghost == vehicle && self.owns_participant(connection, persona, *participant)
            })
            .map(|((_, item), _)| *item)
    }
    pub fn owned_identity(
        &self,
        connection: u8,
        persona: u64,
        participant: u16,
    ) -> Option<NamedIdentity> {
        if !self.owns_participant(connection, persona, participant) {
            return None;
        }
        match &self.objects.get(participant)?.initial {
            Initial::Participant(value) => Some(value.identity.clone()),
            _ => None,
        }
    }

    pub fn owned_vehicle(
        &self,
        connection: u8,
        persona: u64,
        participant: u16,
        item: u64,
    ) -> Option<u16> {
        self.owns_participant(connection, persona, participant)
            .then(|| self.vehicles.get(&(participant, item)).copied())
            .flatten()
    }

    /// Remove the participant's vehicle for `item` (garage display car before
    /// its world re-creation). Returns the removed ghost id.
    pub fn remove_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        item: u64,
    ) -> Result<u16, Error> {
        if !self.owns_participant(connection, persona, participant) {
            return Err(Error::UnknownObject);
        }
        let id = *self
            .vehicles
            .get(&(participant, item))
            .ok_or(Error::UnknownObject)?;
        self.remove_unbound_vehicle(participant, id)?;
        self.vehicles.remove(&(participant, item));
        Ok(id)
    }

    fn remove_unbound_vehicle(&mut self, participant: u16, id: u16) -> Result<(), Error> {
        // A caller must publish the detach before deleting a referenced car.
        if matches!(self.objects.get(participant).map(|o| &o.current),
            Some(Update::Participant(p)) if p.entity == Some(id))
        {
            return Err(Error::Shape);
        }
        let mut next = self.objects.clone();
        let object = next.remove(id)?;
        if !matches!(object.initial, Initial::Vehicle { .. }) {
            return Err(Error::TypeMismatch);
        }
        self.objects = next;
        Ok(())
    }

    /// Create one session-only car for this participant, without assigning a
    /// persistent item ID. It shares normal player seats and simulation authority
    /// but cannot be addressed by inventory purchase/equipment operations.
    pub fn create_temporary_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        creation: vehicle::EntityCreation,
        content: &entity::creation::Content,
    ) -> Result<Record, Error> {
        if self.owned_actor(connection, persona, participant).is_none() {
            return Err(Error::UnknownObject);
        }
        if self.temporary_vehicles.contains_key(&participant) {
            return Err(Error::DuplicateObject);
        }
        if creation
            .body
            .creation
            .as_ref()
            .is_none_or(|c| c.connection_id != u16::from(connection))
        {
            return Err(Error::Shape);
        }
        let record = self.objects.spawn_vehicle(creation, content)?;
        self.temporary_vehicles.insert(participant, record.id);
        Ok(record)
    }
    pub fn temporary_vehicle(&self, connection: u8, persona: u64, participant: u16) -> Option<u16> {
        self.owns_participant(connection, persona, participant)
            .then(|| self.temporary_vehicles.get(&participant).copied())
            .flatten()
    }
    pub fn remove_temporary_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
    ) -> Result<u16, Error> {
        let id = self
            .temporary_vehicle(connection, persona, participant)
            .ok_or(Error::UnknownObject)?;
        self.remove_unbound_vehicle(participant, id)?;
        self.temporary_vehicles.remove(&participant);
        Ok(id)
    }
    pub fn create_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        item: u64,
        creation: vehicle::EntityCreation,
        content: &entity::creation::Content,
    ) -> Result<Record, Error> {
        if self.owned_actor(connection, persona, participant).is_none() {
            return Err(Error::UnknownObject);
        }
        if item == 0 {
            return Err(Error::Shape);
        }
        if self.vehicles.contains_key(&(participant, item)) {
            return Err(Error::DuplicateObject);
        }
        let record = self.objects.spawn_vehicle(creation, content)?;
        self.vehicles.insert((participant, item), record.id);
        Ok(record)
    }

    pub fn create_preview_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        slot: u16,
        creation: vehicle::EntityCreation,
        content: &entity::creation::Content,
    ) -> Result<Record, Error> {
        if self.owned_actor(connection, persona, participant).is_none() {
            return Err(Error::UnknownObject);
        }
        if slot > 511
            || self
                .preview_vehicles
                .keys()
                .filter(|(p, _)| *p == participant)
                .count()
                >= 16
        {
            return Err(Error::Bound);
        }
        if self.preview_vehicles.contains_key(&(participant, slot)) {
            return Err(Error::DuplicateObject);
        }
        if creation
            .body
            .creation
            .as_ref()
            .is_none_or(|c| c.connection_id != u16::from(connection))
        {
            return Err(Error::Shape);
        }
        let record = self.objects.spawn_vehicle(creation, content)?;
        self.preview_vehicles.insert((participant, slot), record.id);
        Ok(record)
    }
    pub fn preview_vehicle(
        &self,
        connection: u8,
        persona: u64,
        participant: u16,
        slot: u16,
    ) -> Option<u16> {
        self.owns_participant(connection, persona, participant)
            .then(|| self.preview_vehicles.get(&(participant, slot)).copied())
            .flatten()
    }
    pub fn remove_preview_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        slot: u16,
    ) -> Result<u16, Error> {
        let id = self
            .preview_vehicle(connection, persona, participant, slot)
            .ok_or(Error::UnknownObject)?;
        self.remove_unbound_vehicle(participant, id)?;
        self.preview_vehicles.remove(&(participant, slot));
        Ok(id)
    }

    pub fn staged_vehicle(
        &self,
        connection: u8,
        persona: u64,
        participant: u16,
        item: u64,
    ) -> Option<u16> {
        self.owns_participant(connection, persona, participant)
            .then(|| self.staged_vehicles.get(&(participant, item)).copied())
            .flatten()
    }
    /// Prepare one replacement without changing the driven vehicle binding.
    pub fn stage_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        item: u64,
        creation: vehicle::EntityCreation,
        content: &entity::creation::Content,
    ) -> Result<Record, Error> {
        if self
            .owned_vehicle(connection, persona, participant, item)
            .is_none()
        {
            return Err(Error::UnknownObject);
        }
        if self.staged_vehicles.contains_key(&(participant, item)) {
            return Err(Error::DuplicateObject);
        }
        // At most one replacement for each bounded, existing owned vehicle.
        let record = self.objects.spawn_vehicle(creation, content)?;
        self.staged_vehicles.insert((participant, item), record.id);
        Ok(record)
    }
    /// Commit a prepared replacement only after publishing a participant/view detach.
    pub fn promote_staged_vehicle(
        &mut self,
        connection: u8,
        persona: u64,
        participant: u16,
        item: u64,
    ) -> Result<u16, Error> {
        let id = self
            .staged_vehicle(connection, persona, participant, item)
            .ok_or(Error::UnknownObject)?;
        let removed = self.remove_vehicle(connection, persona, participant, item)?;
        self.staged_vehicles.remove(&(participant, item));
        self.vehicles.insert((participant, item), id);
        Ok(removed)
    }
}
