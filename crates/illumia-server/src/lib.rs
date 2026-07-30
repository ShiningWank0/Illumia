//! illumia-server: illumia-core のサービス層を HTTP (axum) に束ねる薄い層。
//! lib としてもビルドでき、all-in-one デスクトップ版が埋め込んで使う
//! (→ docs/01_architecture.md)。M1 で実装する。

pub use illumia_core::VERSION;
