//! CPU affinity management for optimal performance.
//!
//! This module provides cross-platform CPU affinity management to bind threads
//! to specific CPU cores for better cache locality and reduced context switching.

use crate::{Error, Result};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Global CPU affinity manager
static AFFINITY_MANAGER: Lazy<CpuAffinityManager> = Lazy::new(CpuAffinityManager::new);

/// CPU affinity manager for distributing threads across CPU cores.
pub struct CpuAffinityManager {
    cpu_count: usize,
    next_cpu: AtomicUsize,
}

impl CpuAffinityManager {
    /// Create a new CPU affinity manager.
    fn new() -> Self {
        let cpu_count = num_cpus::get();
        tracing::info!("Detected {} CPU cores", cpu_count);
        
        Self {
            cpu_count,
            next_cpu: AtomicUsize::new(0),
        }
    }

    /// Get the number of available CPU cores.
    pub fn cpu_count(&self) -> usize {
        self.cpu_count
    }

    /// Get the optimal number of worker threads.
    /// Uses all available cores but caps at a reasonable maximum.
    pub fn optimal_worker_count(&self) -> usize {
        self.cpu_count.min(32).max(1)
    }

    /// Assign the current thread to the next available CPU core.
    pub fn assign_current_thread(&self) -> Result<usize> {
        let cpu_id = self.next_cpu.fetch_add(1, Ordering::Relaxed) % self.cpu_count;
        set_thread_affinity(cpu_id)?;
        tracing::debug!("Assigned current thread to CPU core {}", cpu_id);
        Ok(cpu_id)
    }

    /// Spawn a new thread and bind it to a specific CPU core.
    pub fn spawn_on_cpu<F, T>(&self, cpu_id: usize, f: F) -> std::thread::JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let actual_cpu = cpu_id % self.cpu_count;
        std::thread::spawn(move || {
            if let Err(e) = set_thread_affinity(actual_cpu) {
                tracing::warn!("Failed to set CPU affinity for thread: {}", e);
            }
            f()
        })
    }
}

/// Set CPU affinity for the current thread.
#[cfg(target_os = "linux")]
pub fn set_thread_affinity(cpu_id: usize) -> Result<()> {
    use std::mem;
    
    unsafe {
        let mut set: libc::cpu_set_t = mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu_id, &mut set);
        
        let result = libc::sched_setaffinity(0, mem::size_of::<libc::cpu_set_t>(), &set);
        if result != 0 {
            let error = std::io::Error::last_os_error();
            return Err(Error::internal(format!("Failed to set CPU affinity: {}", error)));
        }
    }
    
    Ok(())
}

/// Set CPU affinity for the current thread (Windows implementation).
#[cfg(target_os = "windows")]
pub fn set_thread_affinity(cpu_id: usize) -> Result<()> {
    use std::ptr;
    
    extern "system" {
        fn SetThreadAffinityMask(
            h_thread: *mut std::ffi::c_void,
            dw_thread_affinity_mask: usize,
        ) -> usize;
        fn GetCurrentThread() -> *mut std::ffi::c_void;
    }
    
    unsafe {
        let mask = 1usize << cpu_id;
        let result = SetThreadAffinityMask(GetCurrentThread(), mask);
        if result == 0 {
            return Err(Error::internal("Failed to set CPU affinity on Windows"));
        }
    }
    
    Ok(())
}

/// Set CPU affinity for the current thread (no-op on unsupported platforms).
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn set_thread_affinity(_cpu_id: usize) -> Result<()> {
    // CPU affinity not supported on this platform
    Ok(())
}

/// Get the global CPU affinity manager.
pub fn global_manager() -> &'static CpuAffinityManager {
    &AFFINITY_MANAGER
}

/// Get the number of available CPU cores.
pub fn cpu_count() -> usize {
    global_manager().cpu_count()
}

/// Get the optimal number of worker threads.
pub fn optimal_worker_count() -> usize {
    global_manager().optimal_worker_count()
}

/// Assign the current thread to the next available CPU core.
pub fn assign_current_thread() -> Result<usize> {
    global_manager().assign_current_thread()
}

/// Create a Tokio runtime with CPU affinity optimization.
pub fn create_optimized_runtime() -> Result<tokio::runtime::Runtime> {
    let worker_threads = optimal_worker_count();
    
    tracing::info!("Creating Tokio runtime with {} worker threads and CPU affinity", worker_threads);
    
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("harbr-worker")
        .on_thread_start(|| {
            if let Err(e) = assign_current_thread() {
                tracing::warn!("Failed to set CPU affinity for worker thread: {}", e);
            }
        })
        .enable_all()
        .build()
        .map_err(|e| Error::internal(format!("Failed to create Tokio runtime: {}", e)))
}

/// Configure the current thread for optimal performance.
pub fn optimize_current_thread() -> Result<()> {
    // Set CPU affinity
    assign_current_thread()?;
    
    // Set thread priority on platforms that support it
    #[cfg(unix)]
    {
        unsafe {
            let result = libc::setpriority(libc::PRIO_PROCESS, 0, -10);
            if result != 0 {
                tracing::warn!("Failed to set thread priority: {}", std::io::Error::last_os_error());
            }
        }
    }
    
    #[cfg(windows)]
    {
        extern "system" {
            fn SetThreadPriority(h_thread: *mut std::ffi::c_void, n_priority: i32) -> i32;
            fn GetCurrentThread() -> *mut std::ffi::c_void;
        }
        
        const THREAD_PRIORITY_ABOVE_NORMAL: i32 = 1;
        
        unsafe {
            let result = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
            if result == 0 {
                tracing::warn!("Failed to set thread priority on Windows");
            }
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_count() {
        let count = cpu_count();
        assert!(count > 0, "Should detect at least one CPU core");
        assert!(count <= 256, "Unrealistic number of CPU cores");
    }

    #[test]
    fn test_optimal_worker_count() {
        let count = optimal_worker_count();
        assert!(count > 0, "Should have at least one worker thread");
        assert!(count <= 32, "Should cap worker threads at reasonable maximum");
    }

    #[test]
    fn test_affinity_manager() {
        let manager = CpuAffinityManager::new();
        assert_eq!(manager.cpu_count(), num_cpus::get());
        
        let optimal = manager.optimal_worker_count();
        assert!(optimal > 0);
        assert!(optimal <= manager.cpu_count().min(32));
    }
}