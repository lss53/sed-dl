# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.1.1] - 2025-10-20

### 🔧 Changed

- **Architectural Refactoring**: Refactored the application's core logic into a dedicated `workflows` module to significantly improve modularity, readability, and maintainability.

### ✨ Added

- **Enhanced Testing**: Added a comprehensive suite of end-to-end CLI tests to verify the command-line interface and top-level dispatch logic, increasing confidence in the application's stability.

### 🐛 Fixed

- **Improved Robustness**: Enhanced the network client's error handling to eliminate potential panics and provide better error classification.

## [2.1.0] - 2025-10-19

### 🚀 Changed

- **Unified UI Output**: Standardized all command-line output (info, warnings, errors, success messages) for a consistent and professional user experience. This also lays the groundwork for future features like a `--quiet` mode.
- **Improved Performance**: Significantly improved stability and performance during high-concurrency downloads by fixing a critical issue that could cause the application to become unresponsive.
- **Friendlier User Feedback**: Made user-facing messages more context-aware. For example, when a user-specified video quality is not found, the application now provides a neutral informational message instead of an alarming warning.

### 🐛 Fixed

- **Concurrency Stability**: Fixed a critical ownership bug that could cause the application to crash in batch mode (`-b`, `--batch-file`) after parsing all task inputs.
- **Rate Limiting Resilience**: Corrected a performance issue where handling server rate-limiting (HTTP 429) could degrade the responsiveness of other concurrent tasks.

### 🔧 Refactor

- **Centralized Constants**: Eliminated "magic strings" by replacing hardcoded API keys and path components with centralized constants, improving code robustness and maintainability.
- **Simplified Configuration Model**: Reduced code duplication and simplified the internal configuration structure by merging redundant data models, making the application easier to extend in the future.

## [2.0.0] - 2025-10-18

### ⚠️ BREAKING CHANGES ⚠️

- **Configuration File**: Updated the structure of `config.json` to include new sections for directory structure (`directory_structure`) and to simplify API endpoint definitions. Old configuration files are **not compatible** and will be automatically backed up and replaced with a new default on first run.
- **Command-line Arguments**:
  - The `--prompt-each` flag has been **removed**. Its functionality is now the default behavior in interactive mode.
  - Interactive mode (`-i`, `--interactive`) is now **mutually exclusive** with flags that imply non-interactive selection (`-q`, `--select`). Using them together will now result in an error.
  - The video quality flag (`-q`, `--video-quality`) now **only accepts numeric values** (e.g., `720`). Suffixes like `p` are no longer supported.
- **Output Directory Structure & Filenames**:
  - The resource title is no longer used as the final directory level, resulting in flatter and more direct save paths.
  - For "High School" resources, the 'grade' level (e.g., "高一") is now automatically omitted from the directory path.
  - The filename format for videos has been standardized to `... [720].ts`, removing the trailing `p`.
  - The filename generation logic for `syncClassroom` resources has been corrected to use the "lesson title" for better organization.

### ✨ Added

- **Smart Input Detection**: Implemented a single prompt in interactive mode that automatically detects whether the input is a URL or a resource ID.
- **Advanced Configuration**: Enabled customization of the directory naming scheme and key API parameters directly within `config.json`, making the application more resilient to future API changes.
- **Comprehensive Test Suite**: Introduced an extensive suite of unit and integration tests covering all core logic using a mock server.
- **Smart Rate Limiting Handling**: Implemented intelligent handling of `HTTP 429 Too Many Requests` errors in the HTTP client by respecting the `Retry-After` header.

### 🚀 Changed

- **Improved Batch Mode UI**: Made progress reporting more concise and intuitive, summarizing filter operations with a clear chain (e.g., `(10 -> 5 available)`) and removing redundant messages.
- **Consistent Error Reporting**: Standardized user input errors (e.g., "Resource not found") to be displayed as yellow warnings (`[!]`) across all modes, distinguishing them from critical application errors.
- **Improved Authentication Flow**: The application now attempts downloads first and only prompts for a token if an authentication error occurs.
- **Enhanced Error Reporting**: Differentiated error messages between a **missing token** and an **invalid token**.
- **Intuitive Interactive Menus**: Defaulted all interactive selection menus to the first option, allowing users to simply press Enter.
- **Refined Final Summary**: Removed redundant "some downloads failed" messages in batch mode. Highlighted summary messages for failed tasks in yellow for better visibility.
- Clarified startup messages regarding missing tokens.

### 🐛 Fixed

- **Extractor Robustness**:
  - Corrected the `SyncClassroomExtractor` to handle multiple `res_ref` formats (JSONPath and plain index).
  - Corrected the `TextbookExtractor`'s PDF parsing to rely on the stable `ti_format` field.
  - Fixed an issue in `CourseExtractor` where generic filenames were not correctly replaced by the book's title.
- A bug causing inconsistent sorting of downloadable items between modes.
- A logic error in `select_stream_with_fallback` that could lead to multiple warnings.
- Overly strict M3U8 validation that could cause false negatives.

### 🔧 Refactor

- **Centralized Configuration**: Moved hardcoded values (API keys, path components, internal strategies) into `config.json` and `constants.rs` for improved readability and easier maintenance.
- **Eliminated Code Duplication**:
  - Extracted shared logic for parsing API `res_ref` fields into a common utility function.
  - Unified directory building logic for `Course` and `SyncClassroom` extractors via the `DirectoryBuilder` trait.
- **Increased Robustness**: Replaced all remaining `.unwrap()` and `.expect()` calls in the core application logic with proper `Result` handling.

## [1.3.1] - 2025-10-13

### 🐛 Fixed
- Fixed an issue where `.tmp` files could be left behind if an M3U8 download was interrupted, by implementing an atomic persist operation for temporary files.
- Corrected a bug that could cause duplicate directory names in the save path for course resources.
- Replaced `.unwrap()` calls on config file parsing with `.expect()` to provide clear, user-friendly error messages on panic.

### 🚀 Changed
- Moved network parameters (server prefixes, timeouts, retries) from hardcoded values to `config.json` to allow for user customization.
- Reverted the CLI help message format to a custom layout for better readability and logical grouping of options.

### ⚡ Performance
- Cached textbook tag data on first use via `LazyLock` to avoid repeated computations.
- Reduced unnecessary memory allocations in the HTTP client by preferring `&str` over `String` where applicable.

### 🔧 Refactor
- Extracted the concurrent download task logic into a dedicated helper function (`run_single_concurrent_task`) to improve code clarity in the main download loop.