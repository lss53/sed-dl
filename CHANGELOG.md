# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.3.1] - 2025-10-13

### Fixed
- Fixed an issue where `.tmp` files could be left behind if an M3U8 download was interrupted, by implementing an atomic persist operation for temporary files.
- Corrected a bug that could cause duplicate directory names in the save path for course resources.
- Replaced `.unwrap()` calls on config file parsing with `.expect()` to provide clear, user-friendly error messages on panic.

### Changed
- Moved network parameters (server prefixes, timeouts, retries) from hardcoded values to `config.json` to allow for user customization.
- Reverted the CLI help message format to a custom layout for better readability and logical grouping of options.

### Performance
- Cached textbook tag data on first use via `LazyLock` to avoid repeated computations.
- Reduced unnecessary memory allocations in the HTTP client by preferring `&str` over `String` where applicable.

### Refactor
- Extracted the concurrent download task logic into a dedicated helper function (`run_single_concurrent_task`) to improve code clarity in the main download loop.