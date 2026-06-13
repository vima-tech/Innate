//! Embedded static frontend. Compiled into the binary via `include_str!` — the
//! same pattern as `schema.sql` and the installed `SKILL.md`; no build toolchain,
//! no runtime file dependency.

pub const INDEX_HTML: &str = include_str!("../../assets/web/index.html");
pub const APP_JS: &str = include_str!("../../assets/web/app.js");
pub const STYLE_CSS: &str = include_str!("../../assets/web/style.css");
