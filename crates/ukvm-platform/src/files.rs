//! File ownership and receive-directory policy live behind the platform boundary.
#[cfg(target_os = "linux")]
pub mod history;
#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(all(test, target_os = "linux"))]
mod tests;
