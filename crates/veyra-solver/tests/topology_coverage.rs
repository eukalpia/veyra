use veyra_solver::{
    RoomRelationIndex, RoomTopologyEdge, RoomTopologyRelation, TopologyError,
};

#[test]
fn topology_index_normalizes_and_exposes_every_boundary() {
    assert_eq!(
        RoomRelationIndex::try_new(&[7, 7], &[]),
        Err(TopologyError::DuplicateRoom(7))
    );
    assert_eq!(
        RoomRelationIndex::try_new(
            &[7, 8],
            &[RoomTopologyEdge {
                left_room_id: 99,
                right_room_id: 8,
                relation: RoomTopologyRelation::Near,
            }],
        ),
        Err(TopologyError::UnknownRoom(99))
    );

    let index = RoomRelationIndex::try_new(
        &[7, 8],
        &[RoomTopologyEdge {
            left_room_id: 8,
            right_room_id: 7,
            relation: RoomTopologyRelation::Adjacent,
        }],
    )
    .unwrap_or_else(|_| unreachable!());
    assert!(index.contains_room(7));
    assert!(!index.contains_room(99));
    assert!(index.contains(RoomTopologyRelation::Adjacent, 7, 8));
    assert!(index.contains(RoomTopologyRelation::Adjacent, 8, 7));
    assert!(!index.contains(RoomTopologyRelation::Adjacent, 7, 7));
    assert!(!index.contains(RoomTopologyRelation::Connected, 7, 8));

    for error in [
        TopologyError::DuplicateRoom(7),
        TopologyError::UnknownRoom(9),
        TopologyError::SelfEdge(7),
        TopologyError::DuplicateEdge {
            left_room_id: 7,
            right_room_id: 8,
            relation: RoomTopologyRelation::Near,
        },
    ] {
        assert!(!error.to_string().is_empty());
    }
}
