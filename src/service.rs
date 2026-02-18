//! Service abstraction for remote data access

use crate::message::VariableMetadata;
use std::collections::HashMap;

/// Represents a service on the server
#[derive(Debug, Clone)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub tenant_id: String,
    pub file_path: Option<String>,
    variables: HashMap<String, VariableMetadata>,
}

impl Service {
    /// Create a new service instance
    pub fn new(id: String, name: String, tenant_id: String, file_path: Option<String>) -> Self {
        Self {
            id,
            name,
            tenant_id,
            file_path,
            variables: HashMap::new(),
        }
    }

    /// Get the service ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the service name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the tenant ID
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Check if this service supports direct memory mapping
    pub fn supports_memory_mapping(&self) -> bool {
        self.file_path.is_some()
    }

    /// Get the file path for local memory mapping
    pub fn file_path(&self) -> Option<&str> {
        self.file_path.as_deref()
    }

    /// Add variable metadata
    pub fn add_variable(&mut self, meta: VariableMetadata) {
        self.variables.insert(meta.name.clone(), meta);
    }

    /// Get variable metadata
    pub fn get_variable(&self, name: &str) -> Option<&VariableMetadata> {
        self.variables.get(name)
    }

    /// List all variables
    pub fn variables(&self) -> &HashMap<String, VariableMetadata> {
        &self.variables
    }

    /// Clear variable cache
    pub fn clear_variables(&mut self) {
        self.variables.clear();
    }
}

/// Service manager for client operations
#[derive(Debug)]
pub struct ServiceManager {
    services: HashMap<String, Service>,
}

impl ServiceManager {
    /// Create a new service manager
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
        }
    }

    /// Register a service
    pub fn register(&mut self, service: Service) {
        self.services.insert(service.id.clone(), service);
    }

    /// Get a registered service
    pub fn get(&self, service_id: &str) -> Option<&Service> {
        self.services.get(service_id)
    }

    /// Get a mutable service reference
    pub fn get_mut(&mut self, service_id: &str) -> Option<&mut Service> {
        self.services.get_mut(service_id)
    }

    /// List all registered services
    pub fn list(&self) -> Vec<&Service> {
        self.services.values().collect()
    }

    /// Clear all services
    pub fn clear(&mut self) {
        self.services.clear();
    }

    /// Get service count
    pub fn len(&self) -> usize {
        self.services.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
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
    fn test_service_creation() {
        let service = Service::new(
            "svc_1".to_string(),
            "config".to_string(),
            "tenant_1".to_string(),
            Some("/tmp/service.mmap".to_string()),
        );

        assert_eq!(service.id(), "svc_1");
        assert_eq!(service.name(), "config");
        assert!(service.supports_memory_mapping());
    }

    #[test]
    fn test_service_manager() {
        let mut manager = ServiceManager::new();
        let service = Service::new(
            "svc_1".to_string(),
            "config".to_string(),
            "tenant_1".to_string(),
            None,
        );

        manager.register(service);
        assert_eq!(manager.len(), 1);
        assert!(manager.get("svc_1").is_some());
    }
}
