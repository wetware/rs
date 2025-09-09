use crate::system_capnp::{exporter, importer};
use capnp::capability::{FromClientHook, Promise};
use capnp::private::capability::ClientHook;
use capnp::Error;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

/// Service token type for identifying exported capabilities
/// This matches the Go implementation's ServiceToken type (20 bytes = 160 bits)
pub type ServiceToken = [u8; 20];

/// Type alias for the services map to reduce complexity
/// Using Arc<Mutex<Box<dyn ClientHook>>> to make ClientHook Send+Sync
type ServicesMap = Arc<Mutex<HashMap<ServiceToken, Weak<Mutex<Box<dyn ClientHook>>>>>>;

/// A membrane that manages exported and imported service capabilities.
///
/// The membrane acts as a central registry for service capabilities, allowing
/// services to be exported with unique tokens and later imported by those tokens.
/// It uses weak references to allow automatic cleanup of dropped capabilities.
#[derive(Debug, Clone)]
pub struct Membrane {
    /// Map of service tokens to weak references of capabilities
    services: ServicesMap,
}

impl Membrane {
    /// Create a new membrane instance
    pub fn new() -> Self {
        Self {
            services: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Generate a unique service token
    fn generate_service_token(&self) -> ServiceToken {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);

        // Convert time and counter to 20 bytes (little-endian)
        let mut token = [0u8; 20];

        // Use the first 16 bytes for time (128 bits)
        let time_bytes = time.to_le_bytes();
        for (i, item) in token.iter_mut().enumerate().take(16) {
            *item = time_bytes[i];
        }

        // Use the last 4 bytes for counter (32 bits)
        let counter_bytes = counter.to_le_bytes();
        for (i, item) in token.iter_mut().enumerate().skip(16) {
            *item = counter_bytes[i - 16];
        }

        token
    }

    /// Import a service capability by its token
    pub fn import_service(&self, token: &[u8]) -> Option<Arc<Mutex<Box<dyn ClientHook>>>> {
        if token.len() != 20 {
            return None;
        }

        let mut service_token = [0u8; 20];
        service_token.copy_from_slice(token);

        let services = self.services.lock().unwrap();
        services
            .get(&service_token)
            .and_then(|weak_ref| weak_ref.upgrade())
    }
}

impl exporter::Server for Membrane {
    fn export(
        &mut self,
        _params: exporter::ExportParams,
        mut _results: exporter::ExportResults,
    ) -> Promise<(), Error> {
        let params = _params.get().unwrap();
        let service_reader = params.get_service();
        let service_token = self.generate_service_token();

        // Extract the capability from the service reader
        let capability: crate::system_capnp::exporter::Client =
            match service_reader.get_as_capability() {
                Ok(cap) => cap,
                Err(e) => {
                    return Promise::err(Error::failed(format!("Failed to get capability: {}", e)))
                }
            };

        {
            let mut services = self.services.lock().unwrap();

            // Clean up dead references before adding new ones
            services.retain(|_, weak_ref| weak_ref.upgrade().is_some());

            // Store the capability in the hashmap using the service token as key
            // Wrap ClientHook in Mutex<> to make it Send + Sync
            let capability_hook = capability.into_client_hook();
            let wrapped_capability = Mutex::new(capability_hook);
            let weak_capability = Arc::downgrade(&Arc::new(wrapped_capability));
            services.insert(service_token, weak_capability);
        }

        _results.get().set_service_token(&service_token);
        Promise::ok(())
    }
}

impl importer::Server for Membrane {
    fn import(
        &mut self,
        params: importer::ImportParams,
        mut results: importer::ImportResults,
    ) -> Promise<(), Error> {
        let token = params.get().unwrap().get_service_token().unwrap();

        // Try to get the capability from the membrane
        if let Some(wrapped_capability) = self.import_service(token) {
            // Extract the ClientHook from the Arc<Mutex<Box<dyn ClientHook>>>
            let capability_hook = wrapped_capability.lock().unwrap();
            let capability = capability_hook.clone();

            // Set the capability in the results
            results.get().init_service().set_as_capability(capability);
            Promise::ok(())
        } else {
            Promise::err(Error::failed("Service not found".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_token_uniqueness_under_load() {
        let membrane = Membrane::new();
        let mut tokens = std::collections::HashSet::new();

        // Generate 1000 tokens and ensure they're all unique
        // This tests the time-based generation under load
        for _ in 0..1000 {
            let token = membrane.generate_service_token();
            assert!(tokens.insert(token), "Generated duplicate token");
        }
    }

    #[test]
    fn test_import_service_edge_cases() {
        let membrane = Membrane::new();

        // Test with wrong length token
        let invalid_token = vec![1, 2, 3]; // Only 3 bytes, should be 20
        assert!(membrane.import_service(&invalid_token).is_none());

        // Test with 20-byte token that doesn't exist
        let non_existent_token = [0u8; 20];
        assert!(membrane.import_service(&non_existent_token).is_none());

        // Test with empty slice
        assert!(membrane.import_service(&[]).is_none());

        // Test with exactly 20 bytes of zeros (edge case)
        assert!(membrane.import_service(&[0u8; 20]).is_none());
    }

    #[test]
    fn test_rapid_token_generation() {
        let membrane = Membrane::new();
        let mut tokens = Vec::new();

        // Generate tokens rapidly to test collision resistance
        for _ in 0..1000 {
            let token = membrane.generate_service_token();
            assert_eq!(token.len(), 20);
            tokens.push(token);
        }

        // Verify all tokens are unique
        let unique_tokens: std::collections::HashSet<_> = tokens.into_iter().collect();
        assert_eq!(unique_tokens.len(), 1000);
    }

    #[test]
    fn test_services_map_initial_state() {
        let membrane = Membrane::new();

        // Test that the services map starts empty
        let services = membrane.services.lock().unwrap();
        assert_eq!(services.len(), 0);
    }

    #[test]
    fn test_service_token_time_based_generation() {
        let membrane = Membrane::new();

        // Generate tokens with small delays to test time-based uniqueness
        let mut tokens = Vec::new();
        for _i in 0..10 {
            let token = membrane.generate_service_token();
            tokens.push(token);

            // Small delay to ensure different timestamps
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // All tokens should be unique
        let unique_tokens: std::collections::HashSet<_> = tokens.into_iter().collect();
        assert_eq!(unique_tokens.len(), 10);
    }

    #[test]
    fn test_token_collision_resistance() {
        let membrane = Membrane::new();
        let mut tokens = std::collections::HashSet::new();

        // Generate tokens rapidly to test collision resistance
        // This is important because our token generation uses time-based seeding
        for _ in 0..10000 {
            let token = membrane.generate_service_token();
            assert!(tokens.insert(token), "Token collision detected!");
        }

        // Verify we have exactly 10000 unique tokens
        assert_eq!(tokens.len(), 10000);
    }
}
