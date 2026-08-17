from pathlib import Path

path = Path("crates/veyra-query/src/lib.rs")
text = path.read_text()
old = "        let topology = RoomRelationIndex::try_new(&room_ids, &edges).map_err(QueryError::Topology)?;\n"
new = "        let edges = edges.into_boxed_slice();\n        let topology = RoomRelationIndex::try_new(&room_ids, &edges).map_err(QueryError::Topology)?;\n"
if text.count(old) != 1:
    raise SystemExit("spatial topology constructor anchor changed")
path.write_text(text.replace(old, new, 1))
