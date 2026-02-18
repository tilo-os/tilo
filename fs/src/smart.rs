use std::path::Path;
use crate::hydration::Hydrator;
use crate::metadata::MetadataStore;

/// Heuristics for pre-fetching content based on user access patterns.
pub struct Heuristics {
    hydrator: std::sync::Arc<Hydrator>,
    metadata: std::sync::Arc<std::sync::Mutex<MetadataStore>>,
}

impl Heuristics {
    pub fn new(hydrator: std::sync::Arc<Hydrator>, metadata: std::sync::Arc<std::sync::Mutex<MetadataStore>>) -> Self {
        Self {
            hydrator,
            metadata,
        }
    }

    /// Called when a file is opened.
    pub fn on_open(&self, path: &Path, _flags: i32) {
        // Example Heuristic: If opening a source file (.c, .rs), prefetch related headers/siblings.
        if let Some(ext) = path.extension() {
            if ext == "c" {
                let _header_path = path.with_extension("h");
                // We need to resolve path to inode/hash from metadata store to fetch
                // This is tricky because path resolution is separate in FUSE (inode based).
                // So we assume we can resolve by name in the same directory.
                // For this prototype, we just log intent or try to find a sibling in metadata.
                
                // Real impl: Look up "foo.h" in the parent directory of the opened file.
                // As we don't have full path -> inode mapping easily here without context,
                // we'll skip the exact implementation but this is where it goes.
                // println!("Heuristic: .c opened, prefetching .h: {:?}", header_path);
            }
        }
    }

    /// Called when a directory is read.
    pub fn on_readdir(&self, ino: u64) {
        // Example: Prefetch metadata or content for first few files in directory?
        // Or if it's a "src" directory, prefetch all small text files.
        let store = self.metadata.lock().unwrap();
        if let Some(node) = store.get_node(ino) {
             if let Some(children) = &node.children {
                 // Prefetch first 3 children content if small
                 for &child_ino in children.iter().take(3) {
                     if let Some(child) = store.get_node(child_ino) {
                         if let Some(hash) = &child.hash {
                             // Async prefetch
                             self.hydrator.prefetch(hash.clone());
                         }
                     }
                 }
             }
        }
    }
}
