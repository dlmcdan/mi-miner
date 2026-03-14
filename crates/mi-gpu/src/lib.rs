pub mod manager;

#[cfg(target_os = "macos")]
mod pipeline;
#[cfg(target_os = "macos")]
mod dispatcher;

pub use manager::GpuManager;
