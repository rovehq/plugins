//! Extended filesystem tools for Rove.
//!
//! Provides tools that go beyond the basic read/write/list builtins:
//!   - `patch_file`      — targeted string replacement in a file
//!   - `append_to_file`  — append text to end of file
//!   - `delete_file`     — delete a file
//!   - `file_exists`     — check if a path exists
//!   - `glob_files`      — find files matching a glob pattern (future)
//!   - `grep_files`      — search file contents by regex (future)
//!
//! Currently these tools are compiled into the engine as builtins.
//! This crate is the future home when they become installable native tools.
