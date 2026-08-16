#![forbid(unsafe_code)]

//! Deterministic semantic traveler/party model.
//!
//! Relationships describe people. Rooming constraints describe accommodation intent. The two
//! concepts are deliberately independent: `spouse(A, B)` does not imply `must_same_room(A, B)`.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

const MAX_TRAVELERS: usize = 64;
const MAX_RELATIONSHIPS: usize = 512;
const MAX_GROUPS: usize = 128;
const MAX_GROUP_MEMBERSHIPS: usize = 1_024;
const MAX_GUARDIAN_RELATIONSHIPS: usize = 256;
const MAX_ROOMING_EDGES: usize = 512;
const MAX_AGE: u16 = 130;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TravelerId(u32);
impl TravelerId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GroupId(u32);
impl GroupId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Gregorian civil date used by semantic search. No timezone is involved in occupancy age.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CivilDate {
    year: i32,
    month: u8,
    day: u8,
}

impl CivilDate {
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, PartyError> {
        if !(1..=12).contains(&month) {
            return Err(PartyError::InvalidMonth(month));
        }
        let max_day = days_in_month(year, month);
        if day == 0 || day > max_day {
            return Err(PartyError::InvalidDay { year, month, day });
        }
        Ok(Self { year, month, day })
    }

    #[must_use]
    pub const fn year(self) -> i32 {
        self.year
    }
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    pub fn age_on(self, date: Self) -> Result<u16, PartyError> {
        if date < self {
            return Err(PartyError::CheckInBeforeBirth);
        }
        let mut years = date.year - self.year;
        if (date.month, date.day) < (self.month, self.day) {
            years -= 1;
        }
        let age = u16::try_from(years).map_err(|_| PartyError::AgeOutOfRange)?;
        if age > MAX_AGE {
            return Err(PartyError::AgeOutOfRange);
        }
        Ok(age)
    }
}

const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgeEvidence {
    BirthDate(CivilDate),
    /// Explicit age known to be valid for the requested check-in date.
    AgeAtCheckIn(u16),
}

impl AgeEvidence {
    pub fn age_at(self, check_in: CivilDate) -> Result<u16, PartyError> {
        match self {
            Self::BirthDate(birth_date) => birth_date.age_on(check_in),
            Self::AgeAtCheckIn(age) if age <= MAX_AGE => Ok(age),
            Self::AgeAtCheckIn(_) => Err(PartyError::AgeOutOfRange),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Traveler {
    id: TravelerId,
    age: AgeEvidence,
    accessibility_required: bool,
}

impl Traveler {
    #[must_use]
    pub const fn new(id: TravelerId, age: AgeEvidence, accessibility_required: bool) -> Self {
        Self {
            id,
            age,
            accessibility_required,
        }
    }
    #[must_use]
    pub const fn id(&self) -> TravelerId {
        self.id
    }
    #[must_use]
    pub const fn age_evidence(&self) -> AgeEvidence {
        self.age
    }
    #[must_use]
    pub const fn accessibility_required(&self) -> bool {
        self.accessibility_required
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RelationshipKind {
    Spouse,
    Partner,
    Parent,
    Child,
    Grandparent,
    Sibling,
    Relative,
    Companion,
    Caregiver,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Relationship {
    pub from: TravelerId,
    pub to: TravelerId,
    pub kind: RelationshipKind,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GuardianRelationship {
    pub guardian: TravelerId,
    pub dependent: TravelerId,
    pub valid_for_rooming: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConstraintStrength {
    Must,
    Prefer,
    Avoid,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RoomingRelation {
    SameRoom,
    SeparateRoom,
    Near,
    ConnectedRooms,
    AdjacentRooms,
    SameFloor,
    SameBuilding,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RoomingIntent {
    pub left: TravelerId,
    pub right: TravelerId,
    pub strength: ConstraintStrength,
    pub relation: RoomingRelation,
}

/// Validated party graph. IDs are external query-local semantic IDs, not hot storage UUIDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookingParty {
    travelers: BTreeMap<TravelerId, Traveler>,
    relationships: BTreeSet<Relationship>,
    groups: BTreeMap<GroupId, BTreeSet<TravelerId>>,
    guardians: BTreeSet<GuardianRelationship>,
    rooming: BTreeSet<RoomingIntent>,
}

impl BookingParty {
    #[must_use]
    pub fn builder() -> PartyBuilder {
        PartyBuilder::default()
    }

    #[must_use]
    pub fn traveler(&self, id: TravelerId) -> Option<&Traveler> {
        self.travelers.get(&id)
    }
    #[must_use]
    pub fn travelers(&self) -> impl ExactSizeIterator<Item = &Traveler> {
        self.travelers.values()
    }
    #[must_use]
    pub fn relationships(&self) -> &BTreeSet<Relationship> {
        &self.relationships
    }
    #[must_use]
    pub fn guardians(&self) -> &BTreeSet<GuardianRelationship> {
        &self.guardians
    }
    #[must_use]
    pub fn rooming_intents(&self) -> &BTreeSet<RoomingIntent> {
        &self.rooming
    }
    #[must_use]
    pub fn group_members(&self, group: GroupId) -> Option<&BTreeSet<TravelerId>> {
        self.groups.get(&group)
    }

    pub fn ages_at(&self, check_in: CivilDate) -> Result<Vec<(TravelerId, u16)>, PartyError> {
        self.travelers
            .values()
            .map(|traveler| Ok((traveler.id(), traveler.age_evidence().age_at(check_in)?)))
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct PartyBuilder {
    travelers: BTreeMap<TravelerId, Traveler>,
    relationships: BTreeSet<Relationship>,
    groups: BTreeMap<GroupId, BTreeSet<TravelerId>>,
    guardians: BTreeSet<GuardianRelationship>,
    rooming: BTreeSet<RoomingIntent>,
    memberships: usize,
}

impl PartyBuilder {
    pub fn add_traveler(&mut self, traveler: Traveler) -> Result<&mut Self, PartyError> {
        if self.travelers.len() >= MAX_TRAVELERS && !self.travelers.contains_key(&traveler.id()) {
            return Err(PartyError::TooManyTravelers);
        }
        if self.travelers.insert(traveler.id(), traveler).is_some() {
            return Err(PartyError::DuplicateTraveler);
        }
        Ok(self)
    }

    pub fn add_relationship(
        &mut self,
        relationship: Relationship,
    ) -> Result<&mut Self, PartyError> {
        self.validate_pair(relationship.from, relationship.to)?;
        if self.relationships.len() >= MAX_RELATIONSHIPS
            && !self.relationships.contains(&relationship)
        {
            return Err(PartyError::TooManyRelationships);
        }
        self.relationships.insert(relationship);
        Ok(self)
    }

    pub fn add_group_member(
        &mut self,
        group: GroupId,
        traveler: TravelerId,
    ) -> Result<&mut Self, PartyError> {
        self.require_traveler(traveler)?;
        if !self.groups.contains_key(&group) && self.groups.len() >= MAX_GROUPS {
            return Err(PartyError::TooManyGroups);
        }
        let members = self.groups.entry(group).or_default();
        if !members.contains(&traveler) {
            if self.memberships >= MAX_GROUP_MEMBERSHIPS {
                return Err(PartyError::TooManyGroupMemberships);
            }
            members.insert(traveler);
            self.memberships += 1;
        }
        Ok(self)
    }

    pub fn add_guardian(
        &mut self,
        relationship: GuardianRelationship,
    ) -> Result<&mut Self, PartyError> {
        self.validate_pair(relationship.guardian, relationship.dependent)?;
        if self.guardians.len() >= MAX_GUARDIAN_RELATIONSHIPS
            && !self.guardians.contains(&relationship)
        {
            return Err(PartyError::TooManyGuardianRelationships);
        }
        self.guardians.insert(relationship);
        Ok(self)
    }

    pub fn add_rooming_intent(&mut self, intent: RoomingIntent) -> Result<&mut Self, PartyError> {
        self.validate_pair(intent.left, intent.right)?;
        if self.rooming.len() >= MAX_ROOMING_EDGES && !self.rooming.contains(&intent) {
            return Err(PartyError::TooManyRoomingEdges);
        }
        if contradictory_hard_intent(&self.rooming, intent) {
            return Err(PartyError::ContradictoryHardRoomingIntent);
        }
        self.rooming.insert(intent);
        Ok(self)
    }

    pub fn build(self) -> Result<BookingParty, PartyError> {
        if self.travelers.is_empty() {
            return Err(PartyError::EmptyParty);
        }
        Ok(BookingParty {
            travelers: self.travelers,
            relationships: self.relationships,
            groups: self.groups,
            guardians: self.guardians,
            rooming: self.rooming,
        })
    }

    fn require_traveler(&self, id: TravelerId) -> Result<(), PartyError> {
        if self.travelers.contains_key(&id) {
            Ok(())
        } else {
            Err(PartyError::UnknownTraveler(id))
        }
    }

    fn validate_pair(&self, left: TravelerId, right: TravelerId) -> Result<(), PartyError> {
        self.require_traveler(left)?;
        self.require_traveler(right)?;
        if left == right {
            Err(PartyError::SelfEdge(left))
        } else {
            Ok(())
        }
    }
}

fn contradictory_hard_intent(existing: &BTreeSet<RoomingIntent>, candidate: RoomingIntent) -> bool {
    if candidate.strength != ConstraintStrength::Must {
        return false;
    }
    existing.iter().any(|edge| {
        edge.strength == ConstraintStrength::Must
            && canonical_pair(edge.left, edge.right)
                == canonical_pair(candidate.left, candidate.right)
            && matches!(
                (edge.relation, candidate.relation),
                (RoomingRelation::SameRoom, RoomingRelation::SeparateRoom)
                    | (RoomingRelation::SeparateRoom, RoomingRelation::SameRoom)
            )
    })
}

fn canonical_pair(left: TravelerId, right: TravelerId) -> (TravelerId, TravelerId) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartyError {
    InvalidMonth(u8),
    InvalidDay { year: i32, month: u8, day: u8 },
    CheckInBeforeBirth,
    AgeOutOfRange,
    EmptyParty,
    TooManyTravelers,
    DuplicateTraveler,
    UnknownTraveler(TravelerId),
    SelfEdge(TravelerId),
    TooManyRelationships,
    TooManyGroups,
    TooManyGroupMemberships,
    TooManyGuardianRelationships,
    TooManyRoomingEdges,
    ContradictoryHardRoomingIntent,
}

impl fmt::Display for PartyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for PartyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn date(y: i32, m: u8, d: u8) -> CivilDate {
        CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
    }
    fn traveler(id: u32, birth: CivilDate) -> Traveler {
        Traveler::new(TravelerId::new(id), AgeEvidence::BirthDate(birth), false)
    }

    #[test]
    fn age_is_computed_for_actual_check_in_date() {
        let birth = date(2010, 9, 22);
        assert_eq!(birth.age_on(date(2026, 9, 21)), Ok(15));
        assert_eq!(birth.age_on(date(2026, 9, 22)), Ok(16));
        assert_eq!(AgeEvidence::AgeAtCheckIn(5).age_at(date(2026, 1, 1)), Ok(5));
        assert_eq!(
            AgeEvidence::AgeAtCheckIn(131).age_at(date(2026, 1, 1)),
            Err(PartyError::AgeOutOfRange)
        );
    }

    #[test]
    fn leap_year_and_date_validation_are_deterministic() {
        assert!(CivilDate::new(2024, 2, 29).is_ok());
        assert_eq!(
            CivilDate::new(2023, 2, 29),
            Err(PartyError::InvalidDay {
                year: 2023,
                month: 2,
                day: 29
            })
        );
        assert_eq!(
            CivilDate::new(2026, 13, 1),
            Err(PartyError::InvalidMonth(13))
        );
        assert_eq!(
            CivilDate::new(2026, 1, 0),
            Err(PartyError::InvalidDay {
                year: 2026,
                month: 1,
                day: 0
            })
        );
        assert_eq!(
            date(2026, 1, 2).age_on(date(2026, 1, 1)),
            Err(PartyError::CheckInBeforeBirth)
        );
        assert_eq!(
            (
                date(2026, 3, 4).year(),
                date(2026, 3, 4).month(),
                date(2026, 3, 4).day()
            ),
            (2026, 3, 4)
        );
    }

    #[test]
    fn relationships_groups_guardians_and_rooming_are_independent() {
        let mut builder = BookingParty::builder();
        builder
            .add_traveler(traveler(1, date(1988, 1, 1)))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_traveler(traveler(2, date(1991, 1, 1)))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_traveler(traveler(3, date(2016, 1, 1)))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_relationship(Relationship {
                from: TravelerId::new(1),
                to: TravelerId::new(2),
                kind: RelationshipKind::Spouse,
            })
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_group_member(GroupId::new(7), TravelerId::new(1))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_group_member(GroupId::new(7), TravelerId::new(3))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_guardian(GuardianRelationship {
                guardian: TravelerId::new(1),
                dependent: TravelerId::new(3),
                valid_for_rooming: true,
            })
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(1),
                right: TravelerId::new(2),
                strength: ConstraintStrength::Prefer,
                relation: RoomingRelation::SameRoom,
            })
            .unwrap_or_else(|_| unreachable!());
        let party = builder.build().unwrap_or_else(|_| unreachable!());
        assert_eq!(party.travelers().len(), 3);
        assert_eq!(party.relationships().len(), 1);
        assert_eq!(party.guardians().len(), 1);
        assert_eq!(party.rooming_intents().len(), 1);
        assert_eq!(
            party.group_members(GroupId::new(7)).map(BTreeSet::len),
            Some(2)
        );
        assert!(party.traveler(TravelerId::new(3)).is_some());
        assert_eq!(
            party
                .ages_at(date(2026, 9, 21))
                .unwrap_or_else(|_| unreachable!())
                .len(),
            3
        );
    }

    #[test]
    fn hard_same_and_separate_room_cannot_both_exist() {
        let mut builder = BookingParty::builder();
        builder
            .add_traveler(traveler(1, date(2000, 1, 1)))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_traveler(traveler(2, date(2000, 1, 1)))
            .unwrap_or_else(|_| unreachable!());
        builder
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(1),
                right: TravelerId::new(2),
                strength: ConstraintStrength::Must,
                relation: RoomingRelation::SameRoom,
            })
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            builder
                .add_rooming_intent(RoomingIntent {
                    left: TravelerId::new(2),
                    right: TravelerId::new(1),
                    strength: ConstraintStrength::Must,
                    relation: RoomingRelation::SeparateRoom
                })
                .err(),
            Some(PartyError::ContradictoryHardRoomingIntent)
        );
    }

    #[test]
    fn invalid_graph_edges_fail_closed() {
        let builder = BookingParty::builder();
        assert_eq!(builder.build(), Err(PartyError::EmptyParty));
        let mut builder = BookingParty::builder();
        builder
            .add_traveler(traveler(1, date(2000, 1, 1)))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            builder.add_traveler(traveler(1, date(2001, 1, 1))).err(),
            Some(PartyError::DuplicateTraveler)
        );
        assert_eq!(
            builder
                .add_relationship(Relationship {
                    from: TravelerId::new(1),
                    to: TravelerId::new(9),
                    kind: RelationshipKind::Companion
                })
                .err(),
            Some(PartyError::UnknownTraveler(TravelerId::new(9)))
        );
        assert_eq!(
            builder
                .add_guardian(GuardianRelationship {
                    guardian: TravelerId::new(1),
                    dependent: TravelerId::new(1),
                    valid_for_rooming: true
                })
                .err(),
            Some(PartyError::SelfEdge(TravelerId::new(1)))
        );
    }

    #[test]
    fn accessibility_and_id_getters_are_exact() {
        let traveler = Traveler::new(TravelerId::new(11), AgeEvidence::AgeAtCheckIn(20), true);
        assert_eq!(traveler.id().get(), 11);
        assert_eq!(traveler.age_evidence(), AgeEvidence::AgeAtCheckIn(20));
        assert!(traveler.accessibility_required());
        assert_eq!(GroupId::new(4).get(), 4);
    }

    proptest! {
        #[test]
        fn age_changes_only_after_birthday(
            year in 1900_i32..2020,
            month in 1_u8..13,
            day in 1_u8..29,
            offset in 1_i32..100,
        ) {
            let birth = CivilDate::new(year, month, day).unwrap_or_else(|_| unreachable!());
            let target_year = year + offset;
            let before_day = if day > 1 { day - 1 } else { day };
            let before_month = if day > 1 { month } else if month > 1 { month - 1 } else { month };
            let before = CivilDate::new(target_year, before_month, before_day).unwrap_or_else(|_| unreachable!());
            let on = CivilDate::new(target_year, month, day).unwrap_or_else(|_| unreachable!());
            let on_age = on.year - birth.year;
            prop_assert_eq!(birth.age_on(on), Ok(u16::try_from(on_age).unwrap_or_default()));
            if before < on {
                prop_assert!(birth.age_on(before).unwrap_or_default() <= birth.age_on(on).unwrap_or_default());
            }
        }
    }
}
