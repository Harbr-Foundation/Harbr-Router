// src/plugin/loader.rs - Plugin dynamic loader using libloading
use super::*;
use anyhow::{Context, Result};
use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Plugin loader configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoaderConfig {
    pub plugin_directories: Vec<String>,
    pub auto_reload: bool,
    pub reload_interval_seconds: u64,
    pub max_plugin_memory_mb: usize,
    pub plugin_timeout_seconds: u64,
    pub allowed_plugins: Option<Vec<String>>, // If None, all plugins allowed
    pub blocked_plugins: Vec<String>,
    pub require_signature: bool,
    pub signature_key_path: Option<String>,
}

impl Default for LoaderConfig {
    fn default() -> Self {
        Self {
            plugin_directories: vec!["./plugins".to_string()],
            auto_reload: false,
            reload_interval_seconds: 60,
            max_plugin_memory_mb: 100,
            plugin_timeout_seconds: 30,
            allowed_plugins: None,
            blocked_plugins: Vec::new(),
            require_signature: false,
            signature_key_path: None,
        }
    }
}

/// Plugin load information
#[derive(Debug, Clone)]
pub struct LoadedPluginInfo {
    pub info: PluginInfo,
    pub path: PathBuf,
    pub loaded_at: std::time::SystemTime,
    pub file_size: u64,
    pub file_hash: String,
    pub library_handle: String, // Internal reference
}

/// Plugin loader for dynamic loading from shared libraries
pub struct PluginLoader {
    config: LoaderConfig,
    loaded_libraries: Arc<RwLock<HashMap<String, Library>>>,
    loaded_plugins: Arc<RwLock<HashMap<String, LoadedPluginInfo>>>,
    file_watchers: Arc<RwLock<HashMap<PathBuf, tokio::task::JoinHandle<()>>>>,
}

impl PluginLoader {
    pub fn new(config: LoaderConfig) -> Self {
        Self {
            config,
            loaded_libraries: Arc::new(RwLock::new(HashMap::new())),
            loaded_plugins: Arc::new(RwLock::new(HashMap::new())),
            file_watchers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// Load a plugin from a shared library file
    pub async fn load_plugin<P: AsRef<Path>>(&self, path: P) -> Result<(String, Box<dyn Plugin>)> {
        let path = path.as_ref();
        
        // Validate plugin file
        self.validate_plugin_file(path).await?;
        
        // Load the library
        let lib = unsafe { 
            Library::new(path)
                .with_context(|| format!("Failed to load library: {}", path.display()))?
        };
        
        // Get plugin info first
        let get_info: Symbol<PluginRegisterFn> = unsafe {
            lib.get(b"get_plugin_info")
                .with_context(|| "Plugin must export 'get_plugin_info' function")?
        };
        
        let plugin_info = unsafe { get_info() };
        
        // Validate plugin info
        self.validate_plugin_info(&plugin_info)?;
        
        // Check if plugin is allowed
        if !self.is_plugin_allowed(&plugin_info.name) {
            return Err(anyhow::anyhow!("Plugin '{}' is not allowed", plugin_info.name));
        }
        
        // Get the plugin factory function
        let create_plugin: Symbol<PluginFactory> = unsafe {
            lib.get(b"create_plugin")
                .with_context(|| "Plugin must export 'create_plugin' function")?
        };
        
        // Create the plugin instance
        let plugin_ptr = unsafe { create_plugin() };
        let plugin = unsafe { Box::from_raw(plugin_ptr) };
        
        // Calculate file hash for change detection
        let file_hash = self.calculate_file_hash(path).await?;
        
        // Store library to prevent unloading
        let library_handle = format!("{}_{}", plugin_info.name, plugin_info.version);
        {
            let mut libraries = self.loaded_libraries.write().await;
            libraries.insert(library_handle.clone(), lib);
        }
        
        // Store plugin info
        let loaded_info = LoadedPluginInfo {
            info: plugin_info.clone(),
            path: path.to_path_buf(),
            loaded_at: std::time::SystemTime::now(),
            file_size: std::fs::metadata(path)?.len(),
            file_hash,
            library_handle,
        };
        
        {
            let mut loaded_plugins = self.loaded_plugins.write().await;
            loaded_plugins.insert(plugin_info.name.clone(), loaded_info);
        }
        
        tracing::info!(
            "Loaded plugin '{}' v{} from {}",
            plugin_info.name,
            plugin_info.version,
            path.display()
        );
        
        Ok((plugin_info.name, plugin))
    }
    
    /// Unload a plugin
    pub async fn unload_plugin(&self, plugin_name: &str) -> Result<()> {
        let loaded_info = {
            let mut loaded_plugins = self.loaded_plugins.write().await;
            loaded_plugins.remove(plugin_name)
                .ok_or_else(|| anyhow::anyhow!("Plugin '{}' is not loaded", plugin_name))?
        };
        
        // Remove library
        {
            let mut libraries = self.loaded_libraries.write().await;
            libraries.remove(&loaded_info.library_handle);
        }
        
        // Stop file watcher if exists
        {
            let mut watchers = self.file_watchers.write().await;
            if let Some(handle) = watchers.remove(&loaded_info.path) {
                handle.abort();
            }
        }
        
        tracing::info!("Unloaded plugin '{}'", plugin_name);
        Ok(())
    }
    
    /// Load all plugins from configured directories
    pub async fn load_all_plugins(&self) -> Result<HashMap<String, Box<dyn Plugin>>> {
        let mut loaded_plugins = HashMap::new();
        
        for directory in &self.config.plugin_directories {
            let dir_path = Path::new(directory);
            
            if !dir_path.exists() {
                tracing::warn!("Plugin directory does not exist: {}", directory);
                continue;
            }
            
            let mut entries = tokio::fs::read_dir(dir_path).await
                .with_context(|| format!("Failed to read plugin directory: {}", directory))?;
            
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                
                // Check if it's a shared library
                if self.is_plugin_file(&path) {
                    match self.load_plugin(&path).await {
                        Ok((name, plugin)) => {
                            loaded_plugins.insert(name, plugin);
                        }
                        Err(e) => {
                            tracing::error!("Failed to load plugin from {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }
        
        if self.config.auto_reload {
            self.start_auto_reload().await?;
        }
        
        tracing::info!("Loaded {} plugins", loaded_plugins.len());
        Ok(loaded_plugins)
    }
    
    /// Start auto-reload watcher for plugin files
    pub async fn start_auto_reload(&self) -> Result<()> {
        if !self.config.auto_reload {
            return Ok(());
        }
        
        let loaded_plugins = self.loaded_plugins.read().await.clone();
        let reload_interval = self.config.reload_interval_seconds;
        
        for (_name, loaded_info) in loaded_plugins {
            let path = loaded_info.path.clone();
            let current_hash = loaded_info.file_hash.clone();
            let loader = self.clone_for_reload();
            
            let handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(
                    std::time::Duration::from_secs(reload_interval)
                );
                
                let mut last_hash = current_hash;
                
                loop {
                    interval.tick().await;
                    
                    // Check if file has changed
                    match loader.calculate_file_hash(&path).await {
                        Ok(new_hash) => {
                            if new_hash != last_hash {
                                tracing::info!("Plugin file changed, reloading: {}", path.display());
                                
                                // Reload plugin (this would need to be implemented)
                                // For now, just log the change
                                last_hash = new_hash;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error checking plugin file {}: {}", path.display(), e);
                            break;
                        }
                    }
                }
            });
            
            self.file_watchers.write().await.insert(path, handle);
        }
        
        tracing::info!("Started auto-reload for {} plugins", loaded_plugins.len());
        Ok(())
    }
    
    /// Stop auto-reload watchers
    pub async fn stop_auto_reload(&self) {
        let mut watchers = self.file_watchers.write().await;
        for (_path, handle) in watchers.drain() {
            handle.abort();
        }
        tracing::info!("Stopped auto-reload watchers");
    }
    
    /// Get information about loaded plugins
    pub async fn get_loaded_plugins(&self) -> HashMap<String, LoadedPluginInfo> {
        self.loaded_plugins.read().await.clone()
    }
    
    /// Check if a plugin file is valid
    async fn validate_plugin_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        
        // Check file exists
        if !path.exists() {
            return Err(anyhow::anyhow!("Plugin file does not exist: {}", path.display()));
        }
        
        // Check file extension
        if !self.is_plugin_file(path) {
            return Err(anyhow::anyhow!("Invalid plugin file extension: {}", path.display()));
        }
        
        // Check file size
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("Failed to get metadata for: {}", path.display()))?;
        
        let max_size = (self.config.max_plugin_memory_mb * 1024 * 1024) as u64;
        if metadata.len() > max_size {
            return Err(anyhow::anyhow!(
                "Plugin file too large: {} bytes (max: {} bytes)",
                metadata.len(),
                max_size
            ));
        }
        
        // Check file permissions
        if metadata.permissions().readonly() {
            tracing::warn!("Plugin file is read-only: {}", path.display());
        }
        
        // TODO: Check digital signature if required
        if self.config.require_signature {
            self.verify_plugin_signature(path).await?;
        }
        
        Ok(())
    }
    
    /// Validate plugin information
    fn validate_plugin_info(&self, info: &PluginInfo) -> Result<()> {
        if info.name.is_empty() {
            return Err(anyhow::anyhow!("Plugin name cannot be empty"));
        }
        
        if info.version.is_empty() {
            return Err(anyhow::anyhow!("Plugin version cannot be empty"));
        }
        
        // Validate version format (basic semver check)
        if !info.version.chars().any(|c| c.is_ascii_digit()) {
            return Err(anyhow::anyhow!("Invalid version format: {}", info.version));
        }
        
        // Check minimum router version compatibility
        // TODO: Implement version comparison
        
        Ok(())
    }
    
    /// Check if plugin is allowed to load
    fn is_plugin_allowed(&self, plugin_name: &str) -> bool {
        // Check blocked list first
        if self.config.blocked_plugins.contains(&plugin_name.to_string()) {
            return false;
        }
        
        // Check allowed list if specified
        if let Some(ref allowed) = self.config.allowed_plugins {
            allowed.contains(&plugin_name.to_string())
        } else {
            true // All plugins allowed if no whitelist
        }
    }
    
    /// Check if file is a plugin file based on extension
    fn is_plugin_file<P: AsRef<Path>>(&self, path: P) -> bool {
        path.as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext == "so" || ext == "dylib" || ext == "dll")
            .unwrap_or(false)
    }
    
    /// Calculate file hash for change detection
    async fn calculate_file_hash<P: AsRef<Path>>(&self, path: P) -> Result<String> {
        use sha2::{Sha256, Digest};
        
        let content = tokio::fs::read(path.as_ref()).await
            .with_context(|| format!("Failed to read file: {}", path.as_ref().display()))?;
        
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let hash = hasher.finalize();
        
        Ok(format!("{:x}", hash))
    }
    
    /// Verify plugin digital signature
    async fn verify_plugin_signature<P: AsRef<Path>>(&self, _path: P) -> Result<()> {
        // TODO: Implement digital signature verification
        // This would typically involve:
        // 1. Reading the signature from the plugin file or separate .sig file
        // 2. Verifying the signature against the plugin content
        // 3. Checking against a trusted public key
        
        tracing::warn!("Plugin signature verification not implemented");
        Ok(())
    }
    
    /// Create a clone for reload operations (simplified)
    fn clone_for_reload(&self) -> PluginLoader {
        PluginLoader {
            config: self.config.clone(),
            loaded_libraries: self.loaded_libraries.clone(),
            loaded_plugins: self.loaded_plugins.clone(),
            file_watchers: self.file_watchers.clone(),
        }
    }
    
    /// Get statistics about loaded plugins
    pub async fn get_statistics(&self) -> PluginLoaderStatistics {
        let loaded_plugins = self.loaded_plugins.read().await;
        let libraries = self.loaded_libraries.read().await;
        let watchers = self.file_watchers.read().await;
        
        let total_file_size: u64 = loaded_plugins.values()
            .map(|info| info.file_size)
            .sum();
        
        PluginLoaderStatistics {
            total_plugins: loaded_plugins.len(),
            total_libraries: libraries.len(),
            active_watchers: watchers.len(),
            total_file_size_bytes: total_file_size,
            directories_watched: self.config.plugin_directories.len(),
            auto_reload_enabled: self.config.auto_reload,
        }
    }
    
    /// Cleanup and shutdown
    pub async fn shutdown(&self) -> Result<()> {
        tracing::info!("Shutting down plugin loader...");
        
        // Stop auto-reload
        self.stop_auto_reload().await;
        
        // Unload all libraries
        {
            let mut libraries = self.loaded_libraries.write().await;
            libraries.clear();
        }
        
        // Clear loaded plugins info
        {
            let mut loaded_plugins = self.loaded_plugins.write().await;
            loaded_plugins.clear();
        }
        
        tracing::info!("Plugin loader shut down");
        Ok(())
    }
}

/// Statistics about the plugin loader
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLoaderStatistics {
    pub total_plugins: usize,
    pub total_libraries: usize,
    pub active_watchers: usize,
    pub total_file_size_bytes: u64,
    pub directories_watched: usize,
    pub auto_reload_enabled: bool,
}

/// Plugin scanner for discovering plugins
pub struct PluginScanner {
    directories: Vec<PathBuf>,
}

impl PluginScanner {
    pub fn new(directories: Vec<String>) -> Self {
        Self {
            directories: directories.into_iter().map(PathBuf::from).collect(),
        }
    }
    
    /// Scan for plugin files
    pub async fn scan(&self) -> Result<Vec<PathBuf>> {
        let mut plugin_files = Vec::new();
        
        for directory in &self.directories {
            if !directory.exists() {
                continue;
            }
            
            let mut entries = tokio::fs::read_dir(directory).await?;
            
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                
                if self.is_plugin_file(&path) {
                    plugin_files.push(path);
                }
            }
        }
        
        Ok(plugin_files)
    }
    
    /// Get plugin metadata without loading
    pub async fn get_plugin_metadata<P: AsRef<Path>>(&self, path: P) -> Result<PluginInfo> {
        let path = path.as_ref();
        
        // This is a simplified version - in reality you'd need to load the library
        // temporarily just to get the metadata
        let lib = unsafe { Library::new(path)? };
        
        let get_info: Symbol<PluginRegisterFn> = unsafe {
            lib.get(b"get_plugin_info")?
        };
        
        let info = unsafe { get_info() };
        Ok(info)
    }
    
    fn is_plugin_file<P: AsRef<Path>>(&self, path: P) -> bool {
        path.as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext == "so" || ext == "dylib" || ext == "dll")
            .unwrap_or(false)
    }
}