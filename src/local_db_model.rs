//! Data model definitions for database storage.
//!
//! This module defines the core data structures used for storing and retrieving
//! information from the LMDB database. The primary model is [`LocalDbModel`],
//! which provides a flexible structure for storing arbitrary JSON data with
//! unique identifiers and content hashing.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// A flexible data model for storing structured information in the database.
///
/// `LocalDbModel` serves as the primary data container for all database operations.
/// It consists of a unique identifier, a content hash for integrity verification,
/// and arbitrary JSON data for application-specific information.
///
/// # Structure
///
/// - **id**: Unique identifier used as the database key
/// - **hash**: Content hash for data integrity and change detection
/// - **data**: Arbitrary JSON data containing the actual application data
///
/// # Examples
///
/// ## Creating a new model
///
/// ```rust
/// use offline_first_core::local_db_model::LocalDbModel;
/// use serde_json::json;
///
/// let model = LocalDbModel {
///     id: "user_12345".to_string(),
///     hash: "sha256_content_hash".to_string(),
///     data: json!({
///         "name": "John Doe",
///         "email": "john@example.com",
///         "age": 30,
///         "preferences": {
///             "theme": "dark",
///             "notifications": true
///         }
///     }),
/// };
/// ```
///
/// ## With complex nested data
///
/// ```rust
/// use offline_first_core::local_db_model::LocalDbModel;
/// use serde_json::json;
///
/// let model = LocalDbModel {
///     id: "document_001".to_string(),
///     hash: "doc_hash_abc123".to_string(),
///     data: json!({
///         "title": "Project Documentation",
///         "sections": [
///             {
///                 "name": "Introduction",
///                 "content": "This document describes..."
///             },
///             {
///                 "name": "Architecture",
///                 "content": "The system is designed..."
///             }
///         ],
///         "metadata": {
///             "created_at": "2024-01-15T10:30:00Z",
///             "author": "engineering_team",
///             "version": "1.0.0"
///         }
///     }),
/// };
/// ```
///
/// # Serialization
///
/// The model implements [`Serialize`] and [`Deserialize`] traits from serde,
/// enabling seamless JSON conversion for database storage and FFI operations.
///
/// ```rust
/// use offline_first_core::local_db_model::LocalDbModel;
/// use serde_json::json;
///
/// let model = LocalDbModel {
///     id: "test".to_string(),
///     hash: "test_hash".to_string(),
///     data: json!({"key": "value"}),
/// };
///
/// // Serialize to JSON string
/// let json_string = serde_json::to_string(&model)?;
/// println!("Serialized: {}", json_string);
///
/// // Deserialize from JSON string
/// let deserialized: LocalDbModel = serde_json::from_str(&json_string)?;
/// assert_eq!(model.id, deserialized.id);
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// # Clone Support
///
/// The model implements [`Clone`] for easy duplication when needed for
/// updates or transformations.
///
/// ```rust
/// use offline_first_core::local_db_model::LocalDbModel;
/// use serde_json::json;
///
/// let original = LocalDbModel {
///     id: "original".to_string(),
///     hash: "hash123".to_string(),
///     data: json!({"status": "active"}),
/// };
///
/// let mut updated = original.clone();
/// updated.hash = "new_hash456".to_string();
/// updated.data = json!({"status": "updated"});
/// ```
///
/// # Database Integration
///
/// This model is designed to work seamlessly with the database operations
/// provided by [`AppDbState`]:
///
/// ```no_run
/// use offline_first_core::{local_db_state::AppDbState, local_db_model::LocalDbModel};
/// use serde_json::json;
///
/// let db = AppDbState::init("my_app".to_string())?;
///
/// let model = LocalDbModel {
///     id: "settings_001".to_string(),
///     hash: "settings_hash".to_string(),
///     data: json!({
///         "theme": "dark",
///         "language": "en",
///         "auto_save": true
///     }),
/// };
///
/// // Store the model
/// db.push(model.clone())?;
///
/// // Retrieve it back
/// let retrieved = db.get_by_id("settings_001")?;
/// assert!(retrieved.is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Field Constraints
///
/// ## ID Field
/// - Must be unique within the database
/// - Cannot be empty (LMDB limitation)
/// - Should be descriptive and meaningful to your application
/// - Common patterns: "user_id", "document_id", "config_name"
///
/// ## Hash Field
/// - Typically used for content integrity verification
/// - Can be any string, commonly SHA-256 hashes
/// - Useful for detecting changes in data
/// - Optional but recommended for data integrity
///
/// ## Data Field
/// - Accepts any valid JSON value
/// - Can contain objects, arrays, strings, numbers, booleans, or null
/// - Size limitations apply based on LMDB configuration
/// - Nested structures are fully supported
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LocalDbModel {
    /// Unique identifier for this record.
    ///
    /// This field serves as the primary key for database operations.
    /// It must be unique within the database and cannot be empty.
    ///
    /// # Examples
    ///
    /// ```rust
    /// // Good ID examples
    /// let user_id = "user_12345".to_string();
    /// let config_id = "app_settings".to_string();
    /// let document_id = "doc_2024_01_15_001".to_string();
    /// ```
    pub id: String,

    /// Content hash for data integrity verification.
    ///
    /// This field typically contains a hash of the data content,
    /// useful for change detection and data integrity verification.
    /// While the hash format is flexible, SHA-256 is commonly used.
    ///
    /// # Examples
    ///
    /// ```rust
    /// // Common hash formats
    /// let sha256_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string();
    /// let simple_hash = "content_v1".to_string();
    /// let timestamp_hash = "20240115_103000".to_string();
    /// ```
    pub hash: String,

    /// Arbitrary JSON data containing the actual application information.
    ///
    /// This field can contain any valid JSON structure, providing maximum
    /// flexibility for storing application-specific data. The JSON is stored
    /// as a string in the database and automatically serialized/deserialized.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use serde_json::json;
    ///
    /// // Simple object
    /// let simple_data = json!({"name": "John", "age": 30});
    ///
    /// // Complex nested structure
    /// let complex_data = json!({
    ///     "user": {
    ///         "profile": {
    ///             "name": "Jane Doe",
    ///             "avatar_url": "https://example.com/avatar.jpg"
    ///         },
    ///         "settings": {
    ///             "notifications": true,
    ///             "theme": "dark"
    ///         }
    ///     },
    ///     "metadata": {
    ///         "created_at": "2024-01-15T10:30:00Z",
    ///         "last_updated": "2024-01-15T14:22:00Z"
    ///     }
    /// });
    ///
    /// // Array data
    /// let array_data = json!([
    ///     {"id": 1, "name": "Item 1"},
    ///     {"id": 2, "name": "Item 2"}
    /// ]);
    /// ```
    pub data: JsonValue,
}