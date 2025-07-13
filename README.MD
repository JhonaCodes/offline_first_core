# Native Storage Library

A high-performance local storage library written in Rust, designed for FFI (Foreign Function Interface) integration. This library provides a robust key-value storage solution using **LMDB** (Lightning Memory-Mapped Database), optimized for embedded systems and mobile applications with superior hot restart support.


## About This Library

This library is designed as a fast-to-compile core, intended to be used as a foundational library for projects like Flutter, native channels, or embedded systems that require low-level interactions. It provides an initial abstraction layer that simplifies integration, making it easy to build efficient local data management solutions.
The library exposes a minimal API that helps integrate it as a base for efficient local data storage, enabling fast and easy access to underlying data without adding unnecessary complexity.

## Features

- 🔋 Embedded key-value storage powered by **LMDB**
- 🔒 ACID compliant transactions
- 🚀 Zero-copy reads
- 📱 Optimized for mobile and embedded systems
- 🛡️ Safe FFI interface with hot restart support
- 🔄 Full CRUD operations
- 🧪 Comprehensive error handling
- 🔧 Explicit connection management for Flutter FFI
- 🏆 Battle-tested LMDB engine (used by OpenLDAP, Bitcoin Core)

## Usage

### C FFI Interface

The library exposes the following FFI functions for integration with other languages:

```c
void* create_db(const char* name);
const char* push_data(void* state, const char* json_ptr);
const char* get_by_id(void* state, const char* id);
const char* get_all(void* state);
const char* update_data(void* state, const char* json_ptr);
const char* delete_by_id(void* state, const char* id);
const char* clear_all_records(void* state);
const char* reset_database(void* state, const char* name);
const char* close_database(void* state);  // NEW: For hot restart support
```

### Direct Rust Usage

```rust
use native_storage::{AppDbState, LocalDbModel};

// Initialize database (creates my_database.lmdb directory)
let db = AppDbState::init("my_database".to_string());

// Create data
let model = LocalDbModel {
    id: "unique_id".to_string(),
    // ... other fields
};

// Store data
db.push(model)?;

// Retrieve data
let item = db.get_by_id("unique_id")?;
let all_items = db.get()?;

// Update data
db.update(updated_model)?;

// Delete data
db.delete_by_id("unique_id")?;

// Clear database
db.clear_all_records()?;

// Reset database
db.reset_database("new_name")?;

// Close database connection (useful for hot restart)
db.close_database()?;
```

## Implementation Details

### Core Components

- `AppDbState`: Main database handler
- `LocalDbModel`: Data model for storage
- FFI layer for C interface

### Storage Engine

Uses **LMDB** (Lightning Memory-Mapped Database) with the following configuration:
- Key-value storage with memory mapping
- ACID transactions with superior consistency
- Single named database within environment
- JSON serialization for values
- Directory-based storage (`.lmdb` directories)
- 1GB default map size with auto-growth
- Optimized for FFI and hot restart scenarios

### Data Model

Data is stored as JSON strings with the following characteristics:
- String keys (IDs) - cannot be empty in LMDB
- Serialized JSON values with size limits
- UTF-8 encoding
- Efficient cursor-based iteration
- Memory-mapped access for performance

### Error Handling

- Comprehensive LMDB error handling for all operations
- Safe FFI error propagation with detailed error messages
- Null pointer checks with improved safety
- JSON parsing validation
- **Zero `unwrap()` calls** in production code
- Graceful handling of LMDB-specific limitations

## Building

### Prerequisites

- Rust 1.75.0 or later
- Cargo
- C compiler (for FFI)

### Build Commands

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Generate C headers
cbindgen --output native_storage.h
```

## Cross-Platform Compilation

### Android

#### Prerequisites:
- Install the Android NDK:
```bash
cargo install cargo-ndk
```

#### Build Commands:
```bash
# For 64-bit devices
cargo ndk -t arm64-v8a build --release

# For 32-bit devices
cargo ndk -t armeabi-v7a build --release
```
- Output: `.so` shared library

### iOS

#### Prerequisites:
- Add the required target:
```bash
rustup target add aarch64-apple-ios
```

#### Build Command:
```bash
cargo build --target aarch64-apple-ios --release
```
- Output: `.a` static library

### macOS

#### Prerequisites:
- Add required targets:
```bash
rustup target add x86_64-apple-darwin
rustup target add aarch64-apple-darwin
```

#### Build Commands:
```bash
cargo build --target aarch64-apple-darwin --release
cargo build --target x86_64-apple-darwin --release
```
- Output: `.dylib` shared library

### Windows

#### Build Command:
```bash
cargo build --target x86_64-pc-windows-msvc --release
```
- Output: `.dll` shared library

### Linux

#### Prerequisites:
- Add required target:
```bash
rustup target add x86_64-unknown-linux-gnu
```

#### Build Command:
```bash
cargo build --target x86_64-unknown-linux-gnu --release
```
- Output: `.so` shared library

## Integration

To integrate with your project:

1. Add to your `Cargo.toml`:
```toml
[dependencies]
lmdb = "0.8"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
log = "0.4"
```

2. Link with your application:
```toml
[lib]
name = "native_storage"
crate-type = ["staticlib", "cdylib"]
```

## Used in:
 1. [Flutter local DB](https://pub.dev/packages/flutter_local_db)

## Safety Considerations

- All FFI functions are marked with `#[no_mangle]`
- Proper memory management for C strings
- Safe handling of null pointers
- Transaction integrity protection
- Resource cleanup on drop

## Performance Notes

- Optimized for embedded systems
- Minimal memory footprint
- Efficient JSON serialization
- Transaction batching where possible

## Limitations

- Single named database per environment
- Synchronous API (by design for FFI compatibility)
- JSON-only value storage
- No built-in encryption
- **LMDB-specific**: Empty keys not supported
- **LMDB-specific**: Large values have size restrictions
- Directory-based storage (not single files)

## Contributing

Contributions are welcome! Please ensure:
1. Proper error handling
2. Memory safety
3. FFI compatibility
4. Documentation
5. Test coverage

## Special Thanks

We would like to extend our special thanks to:

- The creators and contributors of [LMDB](https://www.symas.com/lmdb) for their robust and battle-tested database engine
- The creators and contributors of [redb](https://github.com/cberner/redb) for their innovative work that inspired our initial implementation
- The Rust LMDB bindings maintainers for excellent FFI integration

Thank you for your dedication and contribution to the open-source community!

## Migration from redb

**Version 0.3.0** represents a major architectural change from redb to LMDB. This migration was necessary to resolve critical hot restart issues in Flutter FFI scenarios:

### Why We Migrated

1. **Hot Restart Stability**: redb was causing null pointer exceptions during Flutter hot restart cycles
2. **FFI Robustness**: LMDB provides better memory management for FFI use cases
3. **Production Stability**: LMDB is used by major projects (OpenLDAP, Bitcoin Core, Mozilla)
4. **Better Error Handling**: More predictable error scenarios for mobile development

### What Changed

- Database storage format: `.db` files → `.lmdb` directories
- Error types: redb-specific → LMDB-specific
- Connection management: Automatic → Explicit `close_database()` available
- Memory handling: Improved safety and cleanup

### Migration is Automatic

No code changes required for existing Flutter FFI users - the API remains identical.


## License
MIT License - see [LICENSE](https://github.com/JhonaCodes/flutter_local_db/LICENSE)

## Author
Made with ❤️ by [JhonaCode](https://github.com/JhonaCodes)

---

**v0.3.0** - Now powered by LMDB for superior Flutter FFI stability! 🚀