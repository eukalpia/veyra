use core::{cmp::Ordering, fmt};

use veyra_types::{GenerationId, LogSequenceNumber};

const MAX_SNAPSHOT_ID_BYTES: usize = 128;
const MAX_SNAPSHOT_TABLES: usize = 1_024;
const MAX_SNAPSHOT_KEY_BYTES: usize = 4 * 1_024;
const MAX_SNAPSHOT_VALUE_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_SNAPSHOT_ROWS: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BootstrapPhase {
    #[default]
    Empty,
    Snapshotting,
    ReplayingWal,
    Validating,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotDescriptor {
    snapshot_id: String,
    consistent_lsn: LogSequenceNumber,
    schema_version: u16,
    projection_version: u16,
    tables: Vec<u32>,
}

impl SnapshotDescriptor {
    pub fn try_new(
        snapshot_id: impl Into<String>,
        consistent_lsn: LogSequenceNumber,
        schema_version: u16,
        projection_version: u16,
        tables: Vec<u32>,
    ) -> Result<Self, BootstrapError> {
        let snapshot_id = snapshot_id.into();
        if snapshot_id.is_empty()
            || snapshot_id.len() > MAX_SNAPSHOT_ID_BYTES
            || !snapshot_id.is_ascii()
        {
            return Err(BootstrapError::InvalidSnapshotId);
        }
        if consistent_lsn == LogSequenceNumber::ZERO {
            return Err(BootstrapError::InvalidSnapshotLsn);
        }
        if schema_version == 0 || projection_version == 0 {
            return Err(BootstrapError::InvalidVersion);
        }
        if tables.is_empty() || tables.len() > MAX_SNAPSHOT_TABLES {
            return Err(BootstrapError::InvalidTableSet);
        }
        let mut previous = 0_u32;
        for &table_id in &tables {
            if table_id == 0 || table_id <= previous {
                return Err(BootstrapError::InvalidTableSet);
            }
            previous = table_id;
        }
        Ok(Self {
            snapshot_id,
            consistent_lsn,
            schema_version,
            projection_version,
            tables,
        })
    }

    #[must_use]
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    #[must_use]
    pub const fn consistent_lsn(&self) -> LogSequenceNumber {
        self.consistent_lsn
    }

    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub const fn projection_version(&self) -> u16 {
        self.projection_version
    }

    #[must_use]
    pub fn tables(&self) -> &[u32] {
        &self.tables
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BootstrapProgress {
    rows_read: u64,
    durable_lsn: LogSequenceNumber,
    applied_lsn: LogSequenceNumber,
    replayed_lsn: LogSequenceNumber,
    generation_candidate: GenerationId,
}

impl BootstrapProgress {
    #[must_use]
    pub const fn rows_read(self) -> u64 {
        self.rows_read
    }

    #[must_use]
    pub const fn durable_lsn(self) -> LogSequenceNumber {
        self.durable_lsn
    }

    #[must_use]
    pub const fn applied_lsn(self) -> LogSequenceNumber {
        self.applied_lsn
    }

    #[must_use]
    pub const fn replayed_lsn(self) -> LogSequenceNumber {
        self.replayed_lsn
    }

    #[must_use]
    pub const fn generation_candidate(self) -> GenerationId {
        self.generation_candidate
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapState {
    phase: BootstrapPhase,
    descriptor: Option<SnapshotDescriptor>,
    progress: BootstrapProgress,
    validation_high_water: Option<LogSequenceNumber>,
}

impl BootstrapState {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            phase: BootstrapPhase::Empty,
            descriptor: None,
            progress: BootstrapProgress {
                rows_read: 0,
                durable_lsn: LogSequenceNumber::ZERO,
                applied_lsn: LogSequenceNumber::ZERO,
                replayed_lsn: LogSequenceNumber::ZERO,
                generation_candidate: GenerationId::UNPUBLISHED,
            },
            validation_high_water: None,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> BootstrapPhase {
        self.phase
    }

    #[must_use]
    pub const fn progress(&self) -> BootstrapProgress {
        self.progress
    }

    #[must_use]
    pub fn descriptor(&self) -> Option<&SnapshotDescriptor> {
        self.descriptor.as_ref()
    }

    pub fn begin_snapshot(
        &mut self,
        descriptor: SnapshotDescriptor,
        generation_candidate: GenerationId,
    ) -> Result<(), BootstrapError> {
        self.require_phase(BootstrapPhase::Empty)?;
        if generation_candidate == GenerationId::UNPUBLISHED {
            return Err(BootstrapError::InvalidGeneration);
        }
        self.progress.generation_candidate = generation_candidate;
        self.descriptor = Some(descriptor);
        self.phase = BootstrapPhase::Snapshotting;
        Ok(())
    }

    pub fn record_snapshot_rows(
        &mut self,
        snapshot_id: &str,
        rows: u64,
    ) -> Result<(), BootstrapError> {
        self.require_phase(BootstrapPhase::Snapshotting)?;
        self.require_snapshot(snapshot_id)?;
        let next = self
            .progress
            .rows_read
            .checked_add(rows)
            .ok_or(BootstrapError::ProgressOverflow)?;
        if next > MAX_SNAPSHOT_ROWS {
            return Err(BootstrapError::SnapshotRowLimitExceeded);
        }
        self.progress.rows_read = next;
        Ok(())
    }

    pub fn finish_snapshot(&mut self, snapshot_id: &str) -> Result<(), BootstrapError> {
        self.require_phase(BootstrapPhase::Snapshotting)?;
        let consistent_lsn = self.require_snapshot(snapshot_id)?.consistent_lsn();
        self.progress.replayed_lsn = consistent_lsn;
        self.progress.durable_lsn = consistent_lsn;
        self.progress.applied_lsn = consistent_lsn;
        self.phase = BootstrapPhase::ReplayingWal;
        Ok(())
    }

    pub fn replay_wal(
        &mut self,
        snapshot_id: &str,
        start_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        durable_lsn: LogSequenceNumber,
        applied_lsn: LogSequenceNumber,
    ) -> Result<(), BootstrapError> {
        self.require_phase(BootstrapPhase::ReplayingWal)?;
        self.require_snapshot(snapshot_id)?;
        if start_lsn != self.progress.replayed_lsn {
            return Err(BootstrapError::WalGap {
                expected: self.progress.replayed_lsn,
                actual: start_lsn,
            });
        }
        if end_lsn < start_lsn
            || end_lsn < self.progress.replayed_lsn
            || durable_lsn < self.progress.durable_lsn
            || applied_lsn < self.progress.applied_lsn
        {
            return Err(BootstrapError::ProgressRegression);
        }
        if durable_lsn > end_lsn || applied_lsn > durable_lsn {
            return Err(BootstrapError::InvalidWalProgress);
        }
        self.progress.replayed_lsn = end_lsn;
        self.progress.durable_lsn = durable_lsn;
        self.progress.applied_lsn = applied_lsn;
        Ok(())
    }

    pub fn begin_validation(
        &mut self,
        snapshot_id: &str,
        high_water: LogSequenceNumber,
    ) -> Result<(), BootstrapError> {
        self.require_phase(BootstrapPhase::ReplayingWal)?;
        self.require_snapshot(snapshot_id)?;
        if self.progress.replayed_lsn < high_water
            || self.progress.durable_lsn < high_water
            || self.progress.applied_lsn < high_water
        {
            return Err(BootstrapError::CatchupIncomplete {
                high_water,
                applied: self.progress.applied_lsn,
            });
        }
        self.validation_high_water = Some(high_water);
        self.phase = BootstrapPhase::Validating;
        Ok(())
    }

    pub fn mark_ready(&mut self, snapshot_id: &str) -> Result<(), BootstrapError> {
        self.require_phase(BootstrapPhase::Validating)?;
        self.require_snapshot(snapshot_id)?;
        if self.validation_high_water.is_none() {
            return Err(BootstrapError::ValidationMissing);
        }
        self.phase = BootstrapPhase::Ready;
        Ok(())
    }

    pub fn fail(&mut self) {
        self.phase = BootstrapPhase::Failed;
    }

    fn require_phase(&self, expected: BootstrapPhase) -> Result<(), BootstrapError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(BootstrapError::InvalidPhase {
                expected,
                actual: self.phase,
            })
        }
    }

    fn require_snapshot(&self, snapshot_id: &str) -> Result<&SnapshotDescriptor, BootstrapError> {
        let descriptor = self
            .descriptor
            .as_ref()
            .ok_or(BootstrapError::SnapshotMismatch)?;
        if descriptor.snapshot_id() != snapshot_id {
            return Err(BootstrapError::SnapshotMismatch);
        }
        Ok(descriptor)
    }
}

impl Default for BootstrapState {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotRow {
    table_id: u32,
    key: Vec<u8>,
    value: Vec<u8>,
}

impl SnapshotRow {
    pub fn try_new(table_id: u32, key: Vec<u8>, value: Vec<u8>) -> Result<Self, BootstrapError> {
        if table_id == 0 || key.is_empty() {
            return Err(BootstrapError::InvalidSnapshotRow);
        }
        if key.len() > MAX_SNAPSHOT_KEY_BYTES {
            return Err(BootstrapError::SnapshotKeyTooLarge(key.len()));
        }
        if value.len() > MAX_SNAPSHOT_VALUE_BYTES {
            return Err(BootstrapError::SnapshotValueTooLarge(value.len()));
        }
        Ok(Self {
            table_id,
            key,
            value,
        })
    }

    #[must_use]
    pub const fn table_id(&self) -> u32 {
        self.table_id
    }

    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotAppend {
    Applied,
    Duplicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotSink {
    descriptor: SnapshotDescriptor,
    table_index: usize,
    last_row: Option<SnapshotRow>,
    row_count: u64,
    aborted: bool,
}

impl SnapshotSink {
    pub fn begin(descriptor: SnapshotDescriptor) -> Result<Self, BootstrapError> {
        if descriptor.tables().is_empty() {
            return Err(BootstrapError::InvalidTableSet);
        }
        Ok(Self {
            descriptor,
            table_index: 0,
            last_row: None,
            row_count: 0,
            aborted: false,
        })
    }

    pub fn append(&mut self, row: SnapshotRow) -> Result<SnapshotAppend, BootstrapError> {
        self.ensure_active()?;
        let expected = self.expected_table()?;
        if row.table_id != expected {
            return Err(BootstrapError::UnexpectedSnapshotTable {
                expected,
                actual: row.table_id,
            });
        }
        if let Some(previous) = &self.last_row {
            match row.key.cmp(&previous.key) {
                Ordering::Less => {
                    return Err(BootstrapError::SnapshotKeyOrder {
                        table_id: row.table_id,
                    });
                }
                Ordering::Equal => {
                    if row.value == previous.value {
                        return Ok(SnapshotAppend::Duplicate);
                    }
                    return Err(BootstrapError::ConflictingSnapshotRow {
                        table_id: row.table_id,
                    });
                }
                Ordering::Greater => {}
            }
        }
        if self.row_count >= MAX_SNAPSHOT_ROWS {
            return Err(BootstrapError::SnapshotRowLimitExceeded);
        }
        self.row_count += 1;
        self.last_row = Some(row);
        Ok(SnapshotAppend::Applied)
    }

    pub fn complete_table(&mut self, table_id: u32) -> Result<(), BootstrapError> {
        self.ensure_active()?;
        let expected = self.expected_table()?;
        if table_id != expected {
            return Err(BootstrapError::UnexpectedSnapshotTable {
                expected,
                actual: table_id,
            });
        }
        self.table_index += 1;
        self.last_row = None;
        Ok(())
    }

    pub fn finish(self) -> Result<SnapshotCompletion, BootstrapError> {
        if self.aborted {
            return Err(BootstrapError::SnapshotAborted);
        }
        if self.table_index != self.descriptor.tables().len() {
            return Err(BootstrapError::SnapshotIncomplete);
        }
        Ok(SnapshotCompletion {
            replay_after_lsn: self.descriptor.consistent_lsn(),
            row_count: self.row_count,
        })
    }

    pub fn abort(&mut self) {
        self.aborted = true;
        self.last_row = None;
    }

    fn ensure_active(&self) -> Result<(), BootstrapError> {
        if self.aborted {
            Err(BootstrapError::SnapshotAborted)
        } else {
            Ok(())
        }
    }

    fn expected_table(&self) -> Result<u32, BootstrapError> {
        self.descriptor
            .tables()
            .get(self.table_index)
            .copied()
            .ok_or(BootstrapError::SnapshotAlreadyComplete)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotCompletion {
    replay_after_lsn: LogSequenceNumber,
    row_count: u64,
}

impl SnapshotCompletion {
    #[must_use]
    pub const fn replay_after_lsn(self) -> LogSequenceNumber {
        self.replay_after_lsn
    }

    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapError {
    InvalidSnapshotId,
    InvalidSnapshotLsn,
    InvalidVersion,
    InvalidTableSet,
    InvalidGeneration,
    InvalidPhase {
        expected: BootstrapPhase,
        actual: BootstrapPhase,
    },
    SnapshotMismatch,
    ProgressOverflow,
    ProgressRegression,
    WalGap {
        expected: LogSequenceNumber,
        actual: LogSequenceNumber,
    },
    InvalidWalProgress,
    CatchupIncomplete {
        high_water: LogSequenceNumber,
        applied: LogSequenceNumber,
    },
    ValidationMissing,
    InvalidSnapshotRow,
    SnapshotKeyTooLarge(usize),
    SnapshotValueTooLarge(usize),
    SnapshotRowLimitExceeded,
    UnexpectedSnapshotTable {
        expected: u32,
        actual: u32,
    },
    SnapshotKeyOrder {
        table_id: u32,
    },
    ConflictingSnapshotRow {
        table_id: u32,
    },
    SnapshotAlreadyComplete,
    SnapshotIncomplete,
    SnapshotAborted,
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for BootstrapError {}
