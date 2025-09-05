use crate::swarm_capnp::{exporter, importer};
use capnp::capability::{FromClientHook, Promise};
use capnp::private::capability::ClientHook;
use capnp::Error;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

/// Type alias for the services map to reduce complexity
type ServicesMap = Arc<Mutex<HashMap<Vec<u8>, Weak<Box<dyn ClientHook>>>>>;

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
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            #[allow(clippy::arc_with_non_send_sync)]
            services: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Generate a unique service token
    fn generate_service_token(&self) -> Vec<u8> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        // Convert time to 8 bytes (little-endian)
        let mut token = vec![0u8; 8];
        for (i, item) in token.iter_mut().enumerate().take(8) {
            *item = ((time >> (i * 8)) & 0xFF) as u8;
        }

        token
    }

    /// Import a service capability by its token
    pub fn import_service(&self, token: &[u8]) -> Option<Arc<Box<dyn ClientHook>>> {
        let services = self.services.lock().unwrap();
        services.get(token).and_then(|weak_ref| weak_ref.upgrade())
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
        let capability: crate::swarm_capnp::exporter::Client =
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
            // Convert the Client capability to a Weak<dyn ClientHook> for storage
            let capability_hook = capability.into_client_hook();
            #[allow(clippy::arc_with_non_send_sync)]
            let weak_capability = Arc::downgrade(&Arc::new(capability_hook));
            services.insert(service_token.clone(), weak_capability);
        }

        _results.get().set_token(&service_token);
        Promise::ok(())
    }
}

impl importer::Server for Membrane {
    fn import(
        &mut self,
        params: importer::ImportParams,
        mut results: importer::ImportResults,
    ) -> Promise<(), Error> {
        let token = params.get().unwrap().get_token().unwrap();

        // Try to get the capability from the membrane
        if let Some(capability) = self.import_service(token) {
            // Set the capability in the results
            let capability_hook = Arc::try_unwrap(capability).unwrap_or_else(|arc| (*arc).clone());
            results
                .get()
                .init_service()
                .set_as_capability(capability_hook);
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
    fn test_membrane_starts_empty() {
        let membrane = Membrane::new();
        let services = membrane.services.lock().unwrap();
        assert_eq!(services.len(), 0);
    }

}
