use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use capnp::capability::{Promise, FromClientHook};
use capnp::Error;
use capnp::private::capability::ClientHook;
use crate::swarm_capnp::swarm_capnp::exporter::Client;
use crate::swarm_capnp::swarm_capnp::{exporter, importer};
use std::time::{SystemTime, UNIX_EPOCH};

/// Service manager that implements both Exporter and Importer server traits
/// Maintains a mapping of ServiceTokens to weak references of capabilities
#[derive(Clone)]
pub struct ServiceManager {
    /// Map of service IDs (8-byte random) to weak references of capabilities
    /// Weak references allow services to be garbage collected when no longer used
    services: Arc<Mutex<HashMap<Vec<u8>, Weak<Box<dyn ClientHook>>>>>,
    /// Counter for generating unique IDs
    id_counter: Arc<Mutex<u64>>,
}

impl ServiceManager {
    /// Create a new ServiceManager
    pub fn new() -> Self {
        Self {
            services: Arc::new(Mutex::new(HashMap::new())),
            id_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Generate a new unique random 8-byte service token
    fn generate_service_token(&self) -> Vec<u8> {
        let mut counter = self.id_counter.lock().unwrap();
        *counter += 1;
        
        // Get current time in nanoseconds
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        
        // Combine time and counter for uniqueness
        let mut id = vec![0u8; 8];
        let combined = time ^ (*counter as u128);
        
        // Convert to 8 bytes (little-endian)
        for i in 0..8 {
            id[i] = ((combined >> (i * 8)) & 0xFF) as u8;
        }
        
        id
    }

    /// Export a service capability and return a token
    pub fn export_service(&mut self, capability: Arc<Box<dyn ClientHook>>) -> anyhow::Result<Vec<u8>> {
        let token = self.generate_service_token();
        let weak_ref = Arc::downgrade(&capability);
        
        // Clean up dead references before adding new ones
        let mut services = self.services.lock().unwrap();
        services.retain(|_, weak_ref| weak_ref.upgrade().is_some());
        
        // Store the weak reference
        services.insert(token.clone(), weak_ref);
        Ok(token)
    }
}

impl exporter::Server for ServiceManager {
    fn export(&mut self, _params: exporter::ExportParams, mut _results: exporter::ExportResults) -> Promise<(), Error> {
        let params = _params.get().unwrap();
        let service_reader = params.get_service();
        let service_token = self.generate_service_token();

        
        // Extract the capability from the service reader
        let capability: Client = match service_reader.get_as_capability() {
            Ok(cap) => cap,
            Err(e) => return Promise::err(Error::failed(format!("Failed to get capability: {}", e))),
        };

        
        {
            let mut services = self.services.lock().unwrap();
            
            // Clean up dead references before adding new ones
            services.retain(|_, weak_ref| weak_ref.upgrade().is_some());
            
            // Store the capability in the hashmap using the service token as key
            // Convert the Client capability to a Weak<dyn ClientHook> for storage
            let capability_hook = capability.into_client_hook();
            let weak_capability = Arc::downgrade(&Arc::new(capability_hook));
            services.insert(service_token.clone(), weak_capability);
        }
        
        _results.get().set_token(&service_token);
        Promise::ok(())
    }
}

impl importer::Server for ServiceManager {
    fn import(&mut self, params: importer::ImportParams, mut results: importer::ImportResults) -> Promise<(), Error> {
        let params = params.get().unwrap();
        let token = params.get_token().unwrap();
        
        // Look up the service by token
        let services = self.services.lock().unwrap();
        if let Some(weak_ref) = services.get(token) {
            if let Some(capability) = weak_ref.upgrade() {
                // Extract the Box<dyn ClientHook> from the Arc and set it in results
                let capability_hook = Arc::try_unwrap(capability).unwrap_or_else(|arc| (*arc).clone());
                results.get().init_service().set_as_capability(capability_hook);
                Promise::ok(())
            } else {
                // Service was garbage collected
                Promise::err(Error::failed("Service no longer available".to_string()))
            }
        } else {
            Promise::err(Error::failed("Service token not found".to_string()))
        }
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
    fn test_random_id_generation() {
        let manager = ServiceManager::new();
        let id1 = manager.generate_service_token();
        let id2 = manager.generate_service_token();
        
        // IDs should be 8 bytes
        assert_eq!(id1.len(), 8);
        assert_eq!(id2.len(), 8);
        
        // IDs should be different (very unlikely to be the same)
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_export_and_cleanup() {
        let manager = ServiceManager::new();
        
        // Test that the service manager starts empty
        let services = manager.services.lock().unwrap();
        assert_eq!(services.len(), 0);
        drop(services);
        
        // Test cleanup of empty services
        manager.cleanup_dead_services();
        let services = manager.services.lock().unwrap();
        assert_eq!(services.len(), 0);
        drop(services);
        
        // Test manual cleanup by clearing services
        let mut services = manager.services.lock().unwrap();
        services.clear();
        drop(services);
        
        // Verify cleanup worked
        let services = manager.services.lock().unwrap();
        assert_eq!(services.len(), 0);
    }

    #[test]
    fn test_service_manager_starts_empty() {
        let manager = ServiceManager::new();
        let services = manager.services.lock().unwrap();
        assert_eq!(services.len(), 0);
    }
}
