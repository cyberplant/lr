//! Functions exposed to Lua plugins. Stub for phase 0.
//!
//! Planned API (phase 3):
//!   register_parser { name=, detect=function, parse=function }
//!   line:get_raw() / line:set_field(k, v) / line:set_severity(s)
//!   line:set_timestamp(epoch_ns)
//!   log_debug(msg)
