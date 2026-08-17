use core::fmt;
use std::collections::BTreeSet;

/// Explicit symmetric relation between two distinct projected rooms.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RoomTopologyRelation {
    Near,
    Adjacent,
    Connected,
}

/// One declared symmetric room-topology edge.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RoomTopologyEdge {
    pub left_room_id: u32,
    pub right_room_id: u32,
    pub relation: RoomTopologyRelation,
}

/// Complete topology index for a declared set of room IDs.
///
/// Absence of an edge means the relation is known to be false. Callers must therefore include
/// every room participating in a solve; the solver validates that invariant before search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomRelationIndex {
    rooms: BTreeSet<u32>,
    edges: BTreeSet<(RoomTopologyRelation, u32, u32)>,
}

impl RoomRelationIndex {
    pub fn try_new(room_ids: &[u32], edges: &[RoomTopologyEdge]) -> Result<Self, TopologyError> {
        let mut rooms = BTreeSet::new();
        for &room_id in room_ids {
            if !rooms.insert(room_id) {
                return Err(TopologyError::DuplicateRoom(room_id));
            }
        }
        let mut normalized = BTreeSet::new();
        for &edge in edges {
            if !rooms.contains(&edge.left_room_id) {
                return Err(TopologyError::UnknownRoom(edge.left_room_id));
            }
            if !rooms.contains(&edge.right_room_id) {
                return Err(TopologyError::UnknownRoom(edge.right_room_id));
            }
            if edge.left_room_id == edge.right_room_id {
                return Err(TopologyError::SelfEdge(edge.left_room_id));
            }
            let (left_room_id, right_room_id) = if edge.left_room_id < edge.right_room_id {
                (edge.left_room_id, edge.right_room_id)
            } else {
                (edge.right_room_id, edge.left_room_id)
            };
            if !normalized.insert((edge.relation, left_room_id, right_room_id)) {
                return Err(TopologyError::DuplicateEdge {
                    left_room_id,
                    right_room_id,
                    relation: edge.relation,
                });
            }
        }
        Ok(Self {
            rooms,
            edges: normalized,
        })
    }

    #[must_use]
    pub fn contains_room(&self, room_id: u32) -> bool {
        self.rooms.contains(&room_id)
    }

    #[must_use]
    pub fn contains(&self, relation: RoomTopologyRelation, left: u32, right: u32) -> bool {
        if left == right {
            return false;
        }
        let pair = if left < right {
            (relation, left, right)
        } else {
            (relation, right, left)
        };
        self.edges.contains(&pair)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyError {
    DuplicateRoom(u32),
    UnknownRoom(u32),
    SelfEdge(u32),
    DuplicateEdge {
        left_room_id: u32,
        right_room_id: u32,
        relation: RoomTopologyRelation,
    },
}

impl fmt::Display for TopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for TopologyError {}
