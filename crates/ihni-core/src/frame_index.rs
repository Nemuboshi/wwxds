use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceFrameIndex(u64);

impl SourceFrameIndex {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BoundaryFrameIndex(NonZeroU64);

impl BoundaryFrameIndex {
    pub fn new(value: SourceFrameIndex) -> Result<Self, BoundaryIndexError> {
        NonZeroU64::new(value.get())
            .map(Self)
            .ok_or(BoundaryIndexError)
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryIndexError;

impl Display for BoundaryIndexError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a boundary must start after source frame 0")
    }
}

impl Error for BoundaryIndexError {}

#[cfg(test)]
mod tests {
    use super::{BoundaryFrameIndex, SourceFrameIndex};

    #[test]
    fn source_frame_zero_is_valid() {
        assert_eq!(SourceFrameIndex::new(0).get(), 0);
    }

    #[test]
    fn boundary_cannot_start_at_frame_zero() {
        assert!(BoundaryFrameIndex::new(SourceFrameIndex::new(0)).is_err());
    }

    #[test]
    fn boundary_at_frame_one_is_valid() {
        assert_eq!(
            BoundaryFrameIndex::new(SourceFrameIndex::new(1))
                .expect("frame one is a valid boundary")
                .get(),
            1
        );
    }
}
