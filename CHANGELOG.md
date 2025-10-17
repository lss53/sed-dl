# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0] - 2025-10-17

### ⚠️ BREAKING CHANGES ⚠️

- **Command-line Arguments**:
  - Interactive mode (`-i`, `--interactive`) is now **mutually exclusive** with flags that imply non-interactive selection (`-q`, `--select`). Using them together will now result in an error.
  - The video quality flag (`-q`, `--video-quality`) now **only accepts numeric values** (e.g., `720`). Suffixes like `p` are no longer supported to ensure parsing consistency.
- **Output Filenames**:
  - The filename format for videos has been standardized to `... [720].ts`, removing the trailing `p` from the quality indicator (e.g., `...[720p].ts` is now `...[720].ts`).
  - The filename generation logic for `syncClassroom` resources has been corrected to use the "lesson title" instead of the repetitive "resource title", resulting in more accurate and better-organized files (e.g., `Course Title[Lesson 1] - Resource.pdf`).

### Added

- **Comprehensive Test Suite**: Introduced an extensive suite of unit and integration tests covering all core logic:
  - Unit tests for utility functions, token resolution, download negotiation, and directory building.
  - Integration tests for all three extractor types (`Course`, `SyncClassroom`, `Textbook`) using a mock server.
  - Integration tests for the HTTP client's network error handling, including rate limiting (`429` errors).
- **Smart Rate Limiting Handling**: The HTTP client can now intelligently handle `HTTP 429 Too Many Requests` errors. It respects the `Retry-After` header from the server, pausing and retrying automatically.

### Changed

- **Improved Authentication Flow**: Implemented an "optimistic" authentication strategy. The application now attempts downloads first and only prompts for a token if the server returns an authentication error, allowing for seamless downloading of public resources.
- **Enhanced Error Reporting**: Error messages are now more precise and context-aware. The application can distinguish between a **missing token** and an **invalid token**, providing clearer feedback in both non-interactive and interactive modes.
- **Intuitive Interactive Menus**: All interactive selection menus (for video quality and audio format) now default to option `"1"`, allowing the user to simply press Enter to select the best/first option.
- **Clarified Startup Messages**: The initial message about a missing token is now a neutral statement of fact, resolving logical inconsistencies with the program's behavior in different modes.

### Fixed

- **Extractor Robustness**:
  - Corrected the `SyncClassroomExtractor` to handle multiple `res_ref` formats (JSONPath and plain index), fixing silent failures on certain courses.
  - Corrected the `TextbookExtractor`'s PDF parsing to rely on the stable `ti_format` field instead of the unreliable `ti_file_flag`, preventing valid PDFs from being ignored.
  - Fixed an issue in `CourseExtractor` where a generic filename like `textbook.pdf` was not being correctly identified and replaced by the book's title.
- A bug causing inconsistent sorting of downloadable items between interactive and non-interactive modes.
- A logic error in `select_stream_with_fallback` that could lead to multiple warnings being printed.
- Overly strict M3U8 validation that could cause false negatives on successful downloads.

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