//! Illumia デスクトップクライアント (egui / eframe) — M6。
//!
//! 配布形態は 2 種 (docs/08_clients.md):
//!
//! - **client-only**: リモートサーバーへ HTTP 接続する。
//! - **all-in-one**: `illumia-core` のサービス層を同プロセスで直接呼ぶ。
//!   **TCP listener を一切持たない**ため、同一端末のブラウザを含め
//!   アプリの外からは一切アクセスできない (docs/01 の必須要件)。
//!
//! UI は [`backend::Backend`] にのみ依存し、どちらのモードかを意識しない。

pub mod app;
pub mod backend;
pub mod layout;
