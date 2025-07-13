# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2025-01-13
- Update documentation

## [0.3.0] - 2025-01-13

### 🔄 **BREAKING CHANGES**
- **Migrated from redb to LMDB** as the underlying storage engine
- Database files now use `.lmdb` directory format instead of single files

### ✨ **Added**
- **New FFI function**: `close_database()` for explicit connection management
- Improved hot restart support for Flutter applications
- Better memory management and resource cleanup
- Enhanced error handling for LMDB-specific cases

### 🛡️ **Security & Stability**
- **Eliminated all `unwrap()` calls** in production code for safer error handling
- **Robust error propagation** with comprehensive LMDB error mapping
- **Null pointer safety** improvements in FFI layer
- **ACID compliance** maintained with LMDB transactions

### 🐛 **Fixed**
- **Hot restart issues** in Flutter FFI integration (primary motivation for migration)
- **Null pointer exceptions** during database reconnections
- **Memory leaks** in long-running applications
- **Connection stability** in development environments

### 🎯 **Why LMDB?**

The migration from redb to LMDB was primarily driven by **Flutter FFI stability issues**:

- **Hot Restart Problems**: redb was causing null pointer exceptions during Flutter hot restart cycles
- **Connection Management**: LMDB provides more robust connection handling for FFI scenarios
- **Production Stability**: LMDB is battle-tested in production environments (used by OpenLDAP, Bitcoin Core)
- **Better FFI Support**: LMDB's C-compatible design works better with Flutter's FFI bridge
- **Memory Efficiency**: Superior memory mapping and resource management

### 🔧 **Technical Improvements**
- Database initialization now uses directory-based storage
- Improved cursor iteration for batch operations
- Better transaction lifecycle management
- Enhanced error messages with specific LMDB error codes
- Comprehensive test coverage (20/20 tests passing)

### 📊 **Performance**
- Maintained zero-copy reads
- Optimized transaction batching
- Efficient memory mapping (1GB default map size)
- Improved concurrent access patterns

### 🔄 **Migration Guide**

**For existing Flutter projects:**
- No FFI interface changes required
- Database files will be automatically converted on first run
- Existing data remains compatible through JSON serialization
- Consider calling `close_database()` before hot restart for optimal performance

**For direct Rust usage:**
- Replace `redb` imports with `lmdb` equivalents
- Database paths now create directories instead of files
- Error types have changed to LMDB-specific variants

### 📋 **API Compatibility**
All existing FFI functions remain unchanged:
- ✅ `create_db()`
- ✅ `push_data()`
- ✅ `get_by_id()`
- ✅ `get_all()`
- ✅ `update_data()`
- ✅ `delete_by_id()`
- ✅ `clear_all_records()`
- ✅ `reset_database()`
- 🆕 `close_database()` - NEW

## [0.2.0] - 2024-XX-XX

### Added
- Enhanced redb-based implementation
- Improved CRUD operations
- Better FFI interface for cross-language integration
- Enhanced JSON serialization support

### Fixed
- Memory safety improvements
- Error handling enhancements
- Performance optimizations

## [0.1.1] - 2024-XX-XX

### Added
- Initial redb-based implementation
- Basic CRUD operations
- FFI interface for cross-language integration
- JSON serialization support

### Fixed
- Memory safety improvements
- Error handling enhancements

## [0.1.0] - 2024-XX-XX

### Added
- Initial release
- Core database functionality
- FFI bindings
- Basic documentation