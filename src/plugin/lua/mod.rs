//! Lua plugin host via `mlua` (LuaJIT). Stub for phase 0; full sandbox + API
//! in phase 3.

pub mod api;

// TODO(phase 3): LuaHost that owns an `mlua::Lua` instance, loads `.lua`
// files from config dirs, and adapts registered Lua parsers into the
// `Plugin` trait.
