use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, GroupId, GuardianRelationship,
    PartyError, Relationship, RelationshipKind, RoomingIntent, RoomingRelation, Traveler,
    TravelerId,
};

fn date(year: i32, month: u8, day: u8) -> CivilDate {
    CivilDate::new(year, month, day).unwrap_or_else(|_| unreachable!())
}

fn traveler(id: u32) -> Traveler {
    Traveler::new(TravelerId::new(id), AgeEvidence::AgeAtCheckIn(30), id == 1)
}

fn full_builder() -> veyra_party::PartyBuilder {
    let mut builder = BookingParty::builder();
    for id in 1..=64 {
        builder
            .add_traveler(traveler(id))
            .unwrap_or_else(|_| unreachable!());
    }
    builder
}

#[test]
fn identifiers_dates_accessors_and_age_fail_closed() {
    let traveler_id = TravelerId::new(7);
    let group_id = GroupId::new(9);
    assert_eq!(traveler_id.get(), 7);
    assert_eq!(group_id.get(), 9);

    let check_in = date(2026, 9, 21);
    assert_eq!(
        (check_in.year(), check_in.month(), check_in.day()),
        (2026, 9, 21)
    );
    assert_eq!(date(2000, 2, 29).age_on(check_in), Ok(26));
    assert_eq!(
        date(1800, 1, 1).age_on(check_in),
        Err(PartyError::AgeOutOfRange)
    );

    let accessible = traveler(1);
    assert!(accessible.accessibility_required());
    assert_eq!(accessible.id(), TravelerId::new(1));
    assert_eq!(accessible.age_evidence().age_at(check_in), Ok(30));

    let mut builder = BookingParty::builder();
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(1),
            AgeEvidence::BirthDate(date(1800, 1, 1)),
            false,
        ))
        .unwrap_or_else(|_| unreachable!());
    let party = builder.build().unwrap_or_else(|_| unreachable!());
    assert_eq!(party.ages_at(check_in), Err(PartyError::AgeOutOfRange));
    assert!(party.relationships().is_empty());
    assert!(party.guardians().is_empty());
    assert!(party.rooming_intents().is_empty());
    assert!(party.group_members(group_id).is_none());
}

#[test]
fn traveler_relationship_and_group_limits_are_enforced() {
    let mut builder = full_builder();
    assert_eq!(
        builder.add_traveler(traveler(65)).map(|_| ()),
        Err(PartyError::TooManyTravelers)
    );
    assert_eq!(
        builder.add_traveler(traveler(64)).map(|_| ()),
        Err(PartyError::DuplicateTraveler)
    );

    let mut inserted = 0_usize;
    'relationships: for from in 1..=64 {
        for to in 1..=64 {
            if from == to {
                continue;
            }
            builder
                .add_relationship(Relationship {
                    from: TravelerId::new(from),
                    to: TravelerId::new(to),
                    kind: RelationshipKind::Companion,
                })
                .unwrap_or_else(|_| unreachable!());
            inserted += 1;
            if inserted == 512 {
                break 'relationships;
            }
        }
    }
    assert_eq!(inserted, 512);
    assert_eq!(
        builder
            .add_relationship(Relationship {
                from: TravelerId::new(20),
                to: TravelerId::new(21),
                kind: RelationshipKind::Caregiver,
            })
            .map(|_| ()),
        Err(PartyError::TooManyRelationships)
    );

    let mut groups = full_builder();
    for group in 1..=128 {
        groups
            .add_group_member(GroupId::new(group), TravelerId::new(1))
            .unwrap_or_else(|_| unreachable!());
    }
    groups
        .add_group_member(GroupId::new(1), TravelerId::new(1))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        groups
            .add_group_member(GroupId::new(129), TravelerId::new(1))
            .map(|_| ()),
        Err(PartyError::TooManyGroups)
    );

    let mut memberships = full_builder();
    for group in 1..=16 {
        for id in 1..=64 {
            memberships
                .add_group_member(GroupId::new(group), TravelerId::new(id))
                .unwrap_or_else(|_| unreachable!());
        }
    }
    assert_eq!(
        memberships
            .add_group_member(GroupId::new(17), TravelerId::new(1))
            .map(|_| ()),
        Err(PartyError::TooManyGroupMemberships)
    );
}

#[test]
fn guardian_rooming_limits_and_reversed_contradictions_are_enforced() {
    let mut guardians = full_builder();
    let mut inserted = 0_usize;
    'guardians: for guardian in 1..=64 {
        for dependent in 1..=64 {
            if guardian == dependent {
                continue;
            }
            guardians
                .add_guardian(GuardianRelationship {
                    guardian: TravelerId::new(guardian),
                    dependent: TravelerId::new(dependent),
                    valid_for_rooming: true,
                })
                .unwrap_or_else(|_| unreachable!());
            inserted += 1;
            if inserted == 256 {
                break 'guardians;
            }
        }
    }
    assert_eq!(inserted, 256);
    assert_eq!(
        guardians
            .add_guardian(GuardianRelationship {
                guardian: TravelerId::new(20),
                dependent: TravelerId::new(21),
                valid_for_rooming: false,
            })
            .map(|_| ()),
        Err(PartyError::TooManyGuardianRelationships)
    );

    let mut rooming = full_builder();
    let mut inserted = 0_usize;
    'rooming: for left in 1..=64 {
        for right in 1..=64 {
            if left == right {
                continue;
            }
            rooming
                .add_rooming_intent(RoomingIntent {
                    left: TravelerId::new(left),
                    right: TravelerId::new(right),
                    strength: ConstraintStrength::Prefer,
                    relation: RoomingRelation::Near,
                })
                .unwrap_or_else(|_| unreachable!());
            inserted += 1;
            if inserted == 512 {
                break 'rooming;
            }
        }
    }
    assert_eq!(inserted, 512);
    assert_eq!(
        rooming
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(20),
                right: TravelerId::new(21),
                strength: ConstraintStrength::Avoid,
                relation: RoomingRelation::SameFloor,
            })
            .map(|_| ()),
        Err(PartyError::TooManyRoomingEdges)
    );

    let mut contradiction = full_builder();
    contradiction
        .add_rooming_intent(RoomingIntent {
            left: TravelerId::new(1),
            right: TravelerId::new(2),
            strength: ConstraintStrength::Must,
            relation: RoomingRelation::SameRoom,
        })
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        contradiction
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(2),
                right: TravelerId::new(1),
                strength: ConstraintStrength::Must,
                relation: RoomingRelation::SeparateRoom,
            })
            .map(|_| ()),
        Err(PartyError::ContradictoryHardRoomingIntent)
    );
}

#[test]
fn every_party_error_has_a_stable_display_surface() {
    for error in [
        PartyError::InvalidMonth(13),
        PartyError::InvalidDay {
            year: 2026,
            month: 2,
            day: 30,
        },
        PartyError::CheckInBeforeBirth,
        PartyError::AgeOutOfRange,
        PartyError::EmptyParty,
        PartyError::TooManyTravelers,
        PartyError::DuplicateTraveler,
        PartyError::UnknownTraveler(TravelerId::new(1)),
        PartyError::SelfEdge(TravelerId::new(2)),
        PartyError::TooManyRelationships,
        PartyError::TooManyGroups,
        PartyError::TooManyGroupMemberships,
        PartyError::TooManyGuardianRelationships,
        PartyError::TooManyRoomingEdges,
        PartyError::ContradictoryHardRoomingIntent,
    ] {
        assert!(!error.to_string().is_empty());
    }
}
