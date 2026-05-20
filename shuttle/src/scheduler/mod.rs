//! Implementations of different scheduling strategies for concurrency testing.

mod annotation;
mod dfs;
mod pct;
mod random;
mod replay;
mod round_robin;
mod uncontrolled_nondeterminism;
mod urw;

// Re-export core scheduler types from shuttle-core
pub(crate) use shuttle_core::scheduler::data;
pub(crate) use shuttle_core::scheduler::serialization;
pub(crate) use shuttle_core::scheduler::ScheduleStep;
pub use shuttle_core::scheduler::{DataSource, RandomDataSource, Schedule, Scheduler, Task, TaskId};

pub use annotation::AnnotationScheduler;
pub use dfs::DfsScheduler;
pub use pct::PctScheduler;
pub use random::RandomScheduler;
pub use replay::ReplayScheduler;
pub use round_robin::RoundRobinScheduler;
pub use uncontrolled_nondeterminism::UncontrolledNondeterminismCheckScheduler;
pub use urw::UrwRandomScheduler;
