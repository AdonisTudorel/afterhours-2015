// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::*;
use crate::replication::pacing;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GoalChanges {
    /// Owned goals, in the actor's command order. Shared pacing is excluded.
    pub goals: Vec<u16>,
    /// Detach, create and assign records precede the deletion list.
    pub records: Vec<Record>,
    pub deleted: Vec<u16>,
}

impl Players {
    /// A pacing group belongs to this connection's participants. Its lifetime is
    /// owned by the event, separately from any one AI's commands.
    pub fn create_ai_pacing_group(
        &mut self,
        connection: u8,
        persona: u64,
        entries: Vec<pacing::Entry>,
    ) -> Result<Record, Error> {
        if entries.is_empty() || entries.len() > MAX_AI + 1 || self.pacing_groups.len() >= MAX_AI {
            return Err(Error::Bound);
        }
        let mut members = std::collections::BTreeSet::new();
        for entry in &entries {
            if !members.insert(entry.participant)
                || !(self.owns_participant(connection, persona, entry.participant)
                    || self.owns_ai_participant(connection, persona, entry.participant))
            {
                return Err(Error::UnknownObject);
            }
        }
        let record = self.objects.spawn(
            Initial::AiPacingGroup,
            Update::AiPacingGroup(pacing::Update {
                entries: Some(entries),
            }),
        )?;
        self.pacing_groups.insert(record.id, (connection, persona));
        Ok(record)
    }

    /// Delete an event's pacing group after every actor releases its reference.
    /// Retry is inert; another session cannot remove it.
    pub fn remove_ai_pacing_group(
        &mut self,
        connection: u8,
        persona: u64,
        group: u16,
    ) -> Result<bool, Error> {
        let Some(&owner) = self.pacing_groups.get(&group) else {
            return Ok(false);
        };
        if owner != (connection, persona) {
            return Err(Error::UnknownObject);
        }
        if self.objects.iter().any(|(_, object)| {
            matches!(&object.current, Update::Actor(actor) if
                actor.references_a.iter().chain(&actor.references_b).flatten().any(|id| *id == group))
        }) {
            return Err(Error::Shape);
        }
        self.objects.remove(group)?;
        self.pacing_groups.remove(&group);
        Ok(true)
    }
    /// Replace an AI's owned commands atomically. Intro uses waypoint plus speed
    /// limit; racing uses waypoint plus shared pacing; completion uses wander plus
    /// speed limit. An empty list releases all commands. A failure leaves old
    /// commands, namespace and allocation serial intact.
    pub fn replace_ai_goals(
        &mut self,
        connection: u8,
        persona: u64,
        key: u64,
        goals: Vec<(Initial, Update)>,
        pacing: Option<u16>,
    ) -> Result<GoalChanges, Error> {
        let instance = self
            .ai
            .get(&(connection, persona, key))
            .ok_or(Error::UnknownObject)?;
        if goals.len() + usize::from(pacing.is_some()) > MAX_REFERENCES
            || (goals.is_empty() && pacing.is_some())
        {
            return Err(Error::Bound);
        }
        if let Some(group) = pacing {
            self.check_pacing_member(connection, persona, instance.ids.participant, group)?;
        }
        let actor = instance.ids.actor;
        let mut next = self.objects.clone();
        let mut out = GoalChanges {
            deleted: instance.goals.clone(),
            ..Default::default()
        };
        out.records.extend(next.change(
            actor,
            Update::Actor(ActorUpdate {
                references_a: Some(Vec::new()),
                references_b: Some(Vec::new()),
                ..Default::default()
            }),
        )?);
        for &goal in &out.deleted {
            next.remove(goal)?;
        }
        let mut serial = self.goal_allocation_serial;
        for (mut initial, update) in goals {
            serial = (serial + 1) & 63;
            match &mut initial {
                Initial::GotoWaypointGoal(g) => {
                    g.allocation_serial = serial;
                }
                Initial::SpeedLimitGoal(g) => {
                    g.allocation_serial = serial;
                }
                Initial::ScriptedInputGoal(g) => {
                    g.allocation_serial = serial;
                }
                Initial::WanderGoal(g) => {
                    g.allocation_serial = serial;
                }
                Initial::TrafficGoal(g) => {
                    g.allocation_serial = serial;
                }
                // Police commands (fork experiment): the target is checked by
                // the world model like every other goal reference.
                Initial::ChaseTargetGoal(g) => {
                    g.allocation_serial = serial;
                }
                Initial::FollowTargetGoal(g) => {
                    g.allocation_serial = serial;
                }
                Initial::LookForLawbreakersGoal(g) => {
                    g.allocation_serial = serial;
                }
                _ => return Err(Error::Unsupported),
            }
            let record = next.spawn(initial, update)?;
            out.goals.push(record.id);
            out.records.push(record);
        }
        if !out.goals.is_empty() {
            out.records.extend(next.change(
                actor,
                Update::Actor(ActorUpdate {
                    flag_a: Some(true),
                    references_a: Some(out.goals.iter().copied().chain(pacing).collect()),
                    ..Default::default()
                }),
            )?);
        }
        self.objects = next;
        self.goal_allocation_serial = serial;
        self.ai
            .get_mut(&(connection, persona, key))
            .ok_or(Error::UnknownObject)?
            .goals = out.goals.clone();
        Ok(out)
    }

    pub(in crate::replication::players) fn check_pacing_member(
        &self,
        connection: u8,
        persona: u64,
        participant: u16,
        group: u16,
    ) -> Result<(), Error> {
        if self.pacing_groups.get(&group) != Some(&(connection, persona)) {
            return Err(Error::UnknownObject);
        }
        let Some(state::Object {
            current:
                Update::AiPacingGroup(pacing::Update {
                    entries: Some(entries),
                }),
            ..
        }) = self.objects.get(group)
        else {
            return Err(Error::TypeMismatch);
        };
        if !entries.iter().any(|e| e.participant == participant)
            || entries.iter().any(|e| {
                !(self.owns_participant(connection, persona, e.participant)
                    || self.owns_ai_participant(connection, persona, e.participant))
            })
        {
            return Err(Error::UnknownObject);
        }
        Ok(())
    }
}
