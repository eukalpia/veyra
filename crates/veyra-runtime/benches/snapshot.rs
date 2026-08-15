use criterion::{Criterion, black_box, criterion_group, criterion_main};
use veyra_runtime::{RuntimeSnapshot, RuntimeState};
use veyra_types::{GenerationId, ProjectionProgress};

fn snapshot_load(criterion: &mut Criterion) {
    let state = RuntimeState::new(RuntimeSnapshot::ready(
        GenerationId::new(1),
        ProjectionProgress::ZERO,
    ));

    criterion.bench_function("runtime_snapshot_load", |bencher| {
        bencher.iter(|| black_box(state.snapshot()));
    });
}

criterion_group!(benches, snapshot_load);
criterion_main!(benches);
