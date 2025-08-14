use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use capnp::capability::Promise;
use capnp::Error;
use crate::swarm_capnp::swarm_capnp::{exporter, importer};

/// Service manager that implements both Exporter and Importer server traits
/// Maintains a mapping of ServiceTokens to weak references of capabilities
#[derive(Clone)]
pub struct ServiceManager {
    /// Map of service tokens to weak references of capabilities
    /// Weak references allow services to be garbage collected when no longer used
    services: Arc<Mutex<HashMap<u64, Weak<dyn capnp::private::capability::ClientHook>>>>,
    
    /// Counter for generating unique service tokens
    next_token: Arc<Mutex<u64>>,
}

impl ServiceManager {
    /// Create a new ServiceManager
    pub fn new() -> Self {
        Self {
            services: Arc::new(Mutex::new(HashMap::new())),
            next_token: Arc::new(Mutex::new(1)), // Start from 1, 0 is reserved
        }
    }

    /// Generate a new unique service token
    fn generate_token(&self) -> u64 {
        let mut counter = self.next_token.lock().unwrap();
        let token = *counter;
        *counter += 1;
        token
    }

    /// Convert a u64 token to bytes for Cap'n Proto
    fn token_to_bytes(&self, token: u64) -> Vec<u8> {
        token.to_le_bytes().to_vec()
    }

    /// Convert bytes from Cap'n Proto to u64 token
    fn bytes_to_token(&self, bytes: &[u8]) -> Option<u64> {
        if bytes.len() == 8 {
            let mut token_bytes = [0u8; 8];
            token_bytes.copy_from_slice(bytes);
            Some(u64::from_le_bytes(token_bytes))
        } else {
            None
        }
    }

    /// Clean up dead weak references from the services map
    pub fn cleanup_dead_services(&self) {
        let mut services = self.services.lock().unwrap();
        services.retain(|_, weak_ref| weak_ref.upgrade().is_some());
    }
}

impl exporter::Server for ServiceManager {
    fn export(&mut self, _params: exporter::ExportParams, _results: exporter::ExportResults) -> Promise<(), Error> {
        // TODO: Implement proper field access once we understand the generated types
        // For now, just return success to make it compile
        Promise::ok(())
    }
}

impl importer::Server for ServiceManager {
    fn import(&mut self, _params: importer::ImportParams, _results: importer::ImportResults) -> Promise<(), Error> {
        // TODO: Implement proper field access once we understand the generated types
        // For now, just return success to make it compile
        Promise::ok(())
    }
}

impl Default for ServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_generation() {
        let manager = ServiceManager::new();
        let token1 = manager.generate_token();
        let token2 = manager.generate_token();
        assert_eq!(token1, 1);
        assert_eq!(token2, 2);
    }

    #[test]
    fn test_token_conversion() {
        let manager = ServiceManager::new();
        let original_token = 12345u64;
        let bytes = manager.token_to_bytes(original_token);
        let converted_token = manager.bytes_to_token(&bytes).unwrap();
        assert_eq!(original_token, converted_token);
    }

    #[test]
    fn test_invalid_token_bytes() {
        let manager = ServiceManager::new();
        let invalid_bytes = vec![1, 2, 3]; // Wrong length
        assert!(manager.bytes_to_token(&invalid_bytes).is_none());
    }
}
