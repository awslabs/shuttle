#![deny(warnings, missing_debug_implementations)]
#![allow(dead_code, clippy::new_without_default)]

pub mod annotations;
pub mod config;
pub mod current;
pub mod future;
pub mod hint;
pub mod runtime;
pub mod scheduler;
pub mod sync_types;
pub mod thread_support;

pub use config::{
    Config, ContinuationFunctionBehavior, FailurePersistence, MaxSteps, UngracefulShutdownConfig,
    UNGRACEFUL_SHUTDOWN_CONFIG,
};
pub use runtime::runner::{PortfolioRunner, Runner};
pub use sync_types::{ResourceSignature, ResourceType};

/// If this environment variable is set, then Shuttle will capture the backtrace of each task and display
/// the backtraces in the panic message.
/// Capturing backtraces is quite expensive, so this should only be set when debugging a failing test.
pub const CAPTURE_BACKTRACE: &str = "SHUTTLE_CAPTURE_BACKTRACE";

/// The random seed used to initialize either the [`crate::scheduler::RandomScheduler`] or
/// [`crate::scheduler::PctScheduler`]
const RANDOM_SEED: &str = "SHUTTLE_RANDOM_SEED";

/// If this is set, then warnings about Shuttle's modelling of weak memory and differences between Shuttle's
/// version of LazyStatic and the regular version of LazyStatic will not be emitted.
pub const SILENCE_WARNINGS: &str = "SHUTTLE_SILENCE_WARNINGS";

/// Used in the annotation scheduler to specify where to write the annotations.
pub const ANNOTATION_FILE: &str = "SHUTTLE_ANNOTATION_FILE";

#[cfg(feature = "annotation")]
pub fn annotation_file() -> String {
    std::env::var(ANNOTATION_FILE).unwrap_or_else(|_| "annotated.json".to_string())
}

pub fn silence_warnings() -> bool {
    std::env::var(SILENCE_WARNINGS).is_ok()
}

pub fn backtrace_enabled() -> bool {
    std::env::var(CAPTURE_BACKTRACE).is_ok()
}

pub fn seed_from_env(fallback_seed: u64) -> u64 {
    let seed_env = std::env::var(RANDOM_SEED);
    match seed_env {
        Ok(s) => match s.as_str().parse::<u64>() {
            Ok(seed) => {
                tracing::info!(
                    "Initializing scheduler with the seed provided by {}: {}",
                    RANDOM_SEED,
                    seed
                );
                seed
            }
            Err(err) => panic!("The seed provided by {RANDOM_SEED} is not a valid u64: {err}"),
        },
        Err(_) => fallback_seed,
    }
}
